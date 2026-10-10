//! Inter prediction sample interpolation (8.4.2.2) and weighted sample prediction (8.4.2.3).
//!
//! Samples are `u16` for every bit depth (8-bit pictures are narrowed when they are output);
//! interpolation intermediates are `i32` and every interpolated / weighted store is clipped to
//! `0..=max` — `Clip1Y` / `Clip1C` of clause 5.7 with `max` = `(1 << BitDepth) - 1`. At
//! `max == 255` every formula below is bit-identical to the original 8-bit code.
//!
//! Chroma geometry follows `ChromaArrayType` (clause 8.4.2.2 step 1, equations 8-227..8-234):
//!
//! - `fx` / `fy` passed to [`mc_chroma`] / [`mc_chroma_win`] are always the **raw luma motion
//!   vector bits** `mv[0] & 7` / `mv[1] & 7` (as the 4:2:0 call site has always derived them).
//!   They are never pre-scaled. With `chroma422 == false` the kernel uses `xFracC = fx`,
//!   `yFracC = fy` (8-229/8-230); with `chroma422 == true` it derives `xFracC = fx` (8-233) and
//!   `yFracC = (fy & 3) << 1` (8-234) — for 4:2:2 the vertical fraction is one of only the even
//!   eighths (quarter-sample positions vertically).
//! - The integer chroma position is passed as `(x, y)`. [`chroma_frac422`] derives it (and the
//!   raw fraction bits) for ChromaArrayType == 2 from the luma partition position and MV, so the
//!   4:2:0 call site's inline derivation stays untouched and the 4:2:2 one cannot drift.
//!
//! The sample formula is the same bilinear product of the four surrounding integer samples for
//! both chroma formats (8-270): only the fractional-position derivation differs between 4:2:0
//! and 4:2:2.

/// A reference plane (used by the plane-based helpers in tests).
#[cfg(test)]
#[derive(Clone, Copy)]
pub struct PlaneRef<'a> {
    pub data: &'a [u16],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

const WIN: usize = 16 + 5;

/// Largest partition side: 16x16 luma (8x16 chroma in 4:2:2). Larger requests are refused.
const MAX_BLOCK: usize = 16;

/// Copy the (bw+5)x(bh+5) window whose top-left is (x-2, y-2) into `win` (stride WIN), clamping
/// coordinates to the picture.
#[cfg(test)]
#[inline]
fn fetch_window(p: PlaneRef, x: i32, y: i32, bw: usize, bh: usize, win: &mut [u16; WIN * WIN]) {
    let x0 = x - 2;
    let y0 = y - 2;
    let ww = bw + 5;
    let wh = bh + 5;
    let maxx = p.width as i32 - 1;
    let maxy = p.height as i32 - 1;
    if x0 >= 0 && y0 >= 0 && x0 + ww as i32 - 1 <= maxx && y0 + wh as i32 - 1 <= maxy {
        for r in 0..wh {
            let src = (y0 as usize + r) * p.stride + x0 as usize;
            win[r * WIN..r * WIN + ww].copy_from_slice(&p.data[src..src + ww]);
        }
    } else {
        for r in 0..wh {
            let yy = (y0 + r as i32).clamp(0, maxy) as usize;
            let row = &p.data[yy * p.stride..yy * p.stride + p.width];
            for c in 0..ww {
                let xx = (x0 + c as i32).clamp(0, maxx) as usize;
                win[r * WIN + c] = row[xx];
            }
        }
    }
}

#[inline(always)]
fn tap6(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
    a - 5 * b + 20 * c + 20 * d - 5 * e + f
}

/// `Clip1Y(v)` / `Clip1C(v)` (clause 5.7): clip to `0..=max` and store as a sample. `max` comes
/// from the stream's bit depth; it is clamped to a sane range first so a hostile value can never
/// make `clamp` panic or truncate a valid sample.
#[inline(always)]
fn clip1(v: i32, max: i32) -> u16 {
    v.clamp(0, max.clamp(0, u16::MAX as i32)) as u16
}

/// Copy `n` samples with a fixed-size move for the common block widths.
#[inline(always)]
fn copy_n(dst: &mut [u16], src: &[u16], n: usize) {
    fn fixed<const N: usize>(dst: &mut [u16], src: &[u16]) {
        if let (Some(d), Some(s)) = (dst.first_chunk_mut::<N>(), src.first_chunk::<N>()) {
            *d = *s;
        }
    }
    match n {
        16 => fixed::<16>(dst, src),
        8 => fixed::<8>(dst, src),
        4 => fixed::<4>(dst, src),
        2 => fixed::<2>(dst, src),
        _ => dst[..n].copy_from_slice(&src[..n]),
    }
}

#[inline(always)]
fn avg(a: u16, b: u16) -> u16 {
    ((a as u32 + b as u32 + 1) >> 1) as u16
}

/// Horizontal 6-tap (unclipped) at every position of `row` for `n` outputs: out[c] uses row[c..c + 6].
#[inline(always)]
fn htap_row(row: &[u16], out: &mut [i32], n: usize) {
    let row = &row[..n + 5];
    for (o, w) in out[..n].iter_mut().zip(row.windows(6)) {
        *o = tap6(w[0] as i32, w[1] as i32, w[2] as i32, w[3] as i32, w[4] as i32, w[5] as i32);
    }
}

/// Vertical 6-tap (unclipped) over six rows at columns 0..n.
#[inline(always)]
fn vtap_row(rows: [&[u16]; 6], out: &mut [i32], n: usize) {
    let [a, b, c, d, e, f] = rows.map(|r| &r[..n]);
    for i in 0..n {
        out[i] = tap6(a[i] as i32, b[i] as i32, c[i] as i32, d[i] as i32, e[i] as i32, f[i] as i32);
    }
}

/// Up-front bounds check for the block loops below (rule: index hostile buffers once, up front).
/// The window must cover `bh + win_extra` rows of `bw + win_extra` samples at stride `ss`, the
/// output `bh` rows of `bw` samples at stride `os`; every addition and multiplication is checked
/// (after the block size itself is bounded). An impossible block size (larger than [`MAX_BLOCK`])
/// or a too-small buffer is refused: the function writes nothing instead of panicking or writing
/// out of bounds.
fn buffers_ok(src_len: usize, ss: usize, out_len: usize, os: usize, bw: usize, bh: usize, win_extra: usize) -> bool {
    if bw == 0 || bh == 0 || bw > MAX_BLOCK || bh > MAX_BLOCK {
        return false;
    }
    let (Some(wrows), Some(wcols)) = (bh.checked_add(win_extra), bw.checked_add(win_extra)) else {
        return false;
    };
    let need_src = (wrows - 1).checked_mul(ss).and_then(|v| v.checked_add(wcols));
    let need_out = (bh - 1).checked_mul(os).and_then(|v| v.checked_add(bw));
    match (need_src, need_out) {
        (Some(ns), Some(no)) => src_len >= ns && out_len >= no,
        _ => false,
    }
}

/// Luma sample interpolation for a bw x bh block (8.4.2.2.1). (x, y) is the integer sample position
/// (block position + (mv >> 2)); (fx, fy) the quarter-sample fraction. Output stride is `os`; the
/// stores clip to `0..=max` (`Clip1Y`).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn mc_luma(p: PlaneRef, x: i32, y: i32, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u16], os: usize, max: i32) {
    let mut win = [0u16; WIN * WIN];
    let (x0, y0) = (x - 2, y - 2);
    let inside = x0 >= 0 && y0 >= 0 && x0 as usize + bw + 5 <= p.width && y0 as usize + bh + 5 <= p.height;
    let (src, ss): (&[u16], usize) = if inside {
        (&p.data[y0 as usize * p.stride + x0 as usize..], p.stride)
    } else {
        fetch_window(p, x, y, bw, bh, &mut win);
        (&win[..], WIN)
    };
    mc_luma_win(src, ss, fx, fy, bw, bh, out, os, max);
}

/// Luma interpolation from a prepared window: `src[0]` is the sample at (x - 2, y - 2) and `ss` the
/// window stride; the window covers (bw + 5) x (bh + 5) samples. The stores clip to `0..=max`;
/// integer-position copies are exact (reference planes are already in `0..=max`).
#[allow(clippy::too_many_arguments)]
pub fn mc_luma_win(src: &[u16], ss: usize, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u16], os: usize, max: i32) {
    if !buffers_ok(src.len(), ss, out.len(), os, bw, bh, 5) {
        return;
    }
    // window row r (0..bh+5) starts at src[r * ss]; sample G of block (r, c) is at window (r + 2, c + 2)
    let wrow = |r: usize| &src[r * ss..r * ss + bw + 5];
    let mut t = [0i32; 16];
    match (fx, fy) {
        (0, 0) => {
            for r in 0..bh {
                copy_n(&mut out[r * os..], &wrow(r + 2)[2..], bw);
            }
        }
        (_, 0) => {
            for r in 0..bh {
                let row = wrow(r + 2);
                htap_row(row, &mut t, bw);
                let o = &mut out[r * os..r * os + bw];
                match fx {
                    1 => {
                        for c in 0..bw {
                            o[c] = clip1(avg(row[c + 2], clip1((t[c] + 16) >> 5, max)) as i32, max);
                        }
                    }
                    2 => {
                        for c in 0..bw {
                            o[c] = clip1((t[c] + 16) >> 5, max);
                        }
                    }
                    _ => {
                        for c in 0..bw {
                            o[c] = clip1(avg(row[c + 3], clip1((t[c] + 16) >> 5, max)) as i32, max);
                        }
                    }
                }
            }
        }
        (0, _) => {
            for r in 0..bh {
                let rows = [wrow(r), wrow(r + 1), wrow(r + 2), wrow(r + 3), wrow(r + 4), wrow(r + 5)].map(|x| &x[2..]);
                vtap_row(rows, &mut t, bw);
                let g = if fy == 1 { rows[2] } else { rows[3] };
                let o = &mut out[r * os..r * os + bw];
                if fy == 2 {
                    for c in 0..bw {
                        o[c] = clip1((t[c] + 16) >> 5, max);
                    }
                } else {
                    for c in 0..bw {
                        o[c] = clip1(avg(g[c], clip1((t[c] + 16) >> 5, max)) as i32, max);
                    }
                }
            }
        }
        (2, _) | (_, 2) => {
            // unclipped horizontal half samples b1 for window rows 0..bh+5
            let mut b1 = [[0i32; 16]; WIN];
            for (r, bt) in b1.iter_mut().enumerate().take(bh + 5) {
                htap_row(wrow(r), bt, bw);
            }
            let mut v = [0i32; 16];
            for r in 0..bh {
                let o = &mut out[r * os..r * os + bw];
                let (b0, b1r, b2, b3, b4, b5) = (&b1[r], &b1[r + 1], &b1[r + 2], &b1[r + 3], &b1[r + 4], &b1[r + 5]);
                for c in 0..bw {
                    t[c] = tap6(b0[c], b1r[c], b2[c], b3[c], b4[c], b5[c]);
                }
                match (fx, fy) {
                    (2, 2) => {
                        for c in 0..bw {
                            o[c] = clip1((t[c] + 512) >> 10, max);
                        }
                    }
                    (2, 1) | (2, 3) => {
                        let bh_ = if fy == 1 { b2 } else { b3 };
                        for c in 0..bw {
                            o[c] = avg(clip1((bh_[c] + 16) >> 5, max), clip1((t[c] + 512) >> 10, max));
                        }
                    }
                    _ => {
                        let off = if fx == 1 { 2 } else { 3 };
                        let rows = [wrow(r), wrow(r + 1), wrow(r + 2), wrow(r + 3), wrow(r + 4), wrow(r + 5)].map(|x| &x[off..]);
                        vtap_row(rows, &mut v, bw);
                        for c in 0..bw {
                            o[c] = avg(clip1((v[c] + 16) >> 5, max), clip1((t[c] + 512) >> 10, max));
                        }
                    }
                }
            }
        }
        _ => {
            // e, g, p, r: average of a horizontal half sample (row r or r + 1) and a vertical one (col c or c + 1)
            let hoff = if fy == 1 { 2 } else { 3 };
            let voff = if fx == 1 { 2 } else { 3 };
            let mut v = [0i32; 16];
            for r in 0..bh {
                htap_row(wrow(r + hoff), &mut t, bw);
                let rows = [wrow(r), wrow(r + 1), wrow(r + 2), wrow(r + 3), wrow(r + 4), wrow(r + 5)].map(|x| &x[voff..]);
                vtap_row(rows, &mut v, bw);
                let o = &mut out[r * os..r * os + bw];
                for c in 0..bw {
                    o[c] = avg(clip1((t[c] + 16) >> 5, max), clip1((v[c] + 16) >> 5, max));
                }
            }
        }
    }
}

/// Chroma integer position and raw fraction bits for ChromaArrayType == 2 (4:2:2): equations
/// 8-231..8-234 with `mvCLX = mvLX` (8-221/8-222; 4:2:2 frame macroblocks copy the luma MV
/// unchanged — there is no vertical scaling) and `SubWidthC = 2`, `SubHeightC = 1`.
///
/// Returns `(xIntC, yIntC, fx, fy)` for a chroma block at luma position `(xal, yal)` with luma
/// motion vector `mv` (quarter-luma-sample units): the chroma integer position is the block's
/// upper-left sample (`xIntC = xal / 2 + (mv[0] >> 3)`, `yIntC = yal + (mv[1] >> 2)`), and
/// `fx`/`fy` are the raw bits `mv & 7` to pass **unchanged** to [`mc_chroma`] / [`mc_chroma_win`]
/// with `chroma422: true` (the kernel derives `xFracC = fx` and `yFracC = (fy & 3) << 1`). The
/// source window is (cw + 1) x (ch + 1) samples at `(xIntC, yIntC)`, exactly as in 4:2:0.
///
/// (The 4:2:0 derivation stays inline at its call site: `xIntC = xal / 2 + (mv[0] >> 3)`,
/// `yIntC = yal / 2 + (mv[1] >> 3)`, `fx = mv[0] & 7`, `fy = mv[1] & 7` — equations 8-227..8-230.)
pub fn chroma_frac422(xal: i32, yal: i32, mv: [i32; 2]) -> (i32, i32, u32, u32) {
    // xAL / SubWidthC: partition positions are non-negative; `>> 1` keeps the floor behaviour of
    // the arithmetic `>>` on the motion vector for any input.
    let x_int = (xal >> 1).wrapping_add(mv[0] >> 3);
    let y_int = yal.wrapping_add(mv[1] >> 2);
    (x_int, y_int, (mv[0] & 7) as u32, (mv[1] & 7) as u32)
}

/// Chroma sample interpolation (8.4.2.2.2) for a bw x bh block at integer position (x, y).
/// `fx`/`fy` are the raw luma MV bits (`mv & 7`), `chroma422` selects the vertical
/// fractional-position derivation (module docs): 4:2:0 uses `yFracC = fy` (8-230), 4:2:2 uses
/// `yFracC = (fy & 3) << 1` (8-234). The sample formula (8-270) is the same bilinear product of
/// the four surrounding integer samples for both; the stores clip to `0..=max` (`Clip1C`).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma(p: PlaneRef, x: i32, y: i32, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u16], os: usize, max: i32, chroma422: bool) {
    if bw == 0 || bh == 0 {
        return;
    }
    let (Some(w), Some(h)) = (bw.checked_add(1), bh.checked_add(1)) else {
        return;
    };
    let Some(win_len) = w.checked_mul(h) else {
        return;
    };
    let maxx = p.width as i32 - 1;
    let maxy = p.height as i32 - 1;
    let inside = x >= 0 && y >= 0 && x + bw as i32 <= maxx && y + bh as i32 <= maxy;
    // (bw+1) x (bh+1) source window; copied with edge clamping when it crosses the picture border
    let mut win = vec![0u16; win_len];
    let (src, ss): (&[u16], usize) = if inside {
        (&p.data[y as usize * p.stride + x as usize..], p.stride)
    } else {
        for r in 0..=bh {
            let yy = (y + r as i32).clamp(0, maxy) as usize;
            for c in 0..=bw {
                if let Some(v) = win.get_mut(r * (bw + 1) + c) {
                    *v = p.data[yy * p.stride + (x + c as i32).clamp(0, maxx) as usize];
                }
            }
        }
        (&win[..], bw + 1)
    };
    mc_chroma_win(src, ss, fx, fy, bw, bh, out, os, max, chroma422);
}

/// Chroma interpolation from a prepared (bw + 1) x (bh + 1) window whose first sample is the integer
/// position of the block. `fx`/`fy` are the raw luma MV bits (`mv & 7`); see [`mc_chroma`] for the
/// `chroma422` derivation. Formula 8-270 with `Clip1C` stores.
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma_win(src: &[u16], ss: usize, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u16], os: usize, max: i32, chroma422: bool) {
    if !buffers_ok(src.len(), ss, out.len(), os, bw, bh, 1) {
        return;
    }
    // 8-229/8-233: xFracC is the same for 4:2:0 and 4:2:2. 8-230 vs 8-234: the vertical fraction
    // of 4:2:2 is the low two bits of the luma MV shifted left by one (even eighths only).
    let (fx, fy) = ((fx & 7) as i32, if chroma422 { ((fy & 3) << 1) as i32 } else { (fy & 7) as i32 });
    let (w00, w10, w01, w11) = ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
    if fx == 0 && fy == 0 {
        for r in 0..bh {
            copy_n(&mut out[r * os..], &src[r * ss..], bw);
        }
        return;
    }
    for r in 0..bh {
        let r0 = &src[r * ss..r * ss + bw + 1];
        let r1 = &src[(r + 1) * ss..(r + 1) * ss + bw + 1];
        let o = &mut out[r * os..r * os + bw];
        for (c, o) in o.iter_mut().enumerate() {
            let v = w00 * r0[c] as i32 + w10 * r0[c + 1] as i32 + w01 * r1[c] as i32 + w11 * r1[c + 1] as i32;
            *o = clip1((v + 32) >> 6, max);
        }
    }
}

/// Weights for one prediction direction or pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    /// Default: copy (single list) or rounded average (bi).
    Default,
    /// Explicit/implicit weighting: log2 denominator, weights and offsets for L0 and L1.
    Weighted { log_wd: i32, w0: i32, w1: i32, o0: i32, o1: i32 },
}

/// Combine prediction(s) into `dst` (stride `ds`) per 8.4.2.3, clipping to `0..=max` (`Clip1Y` /
/// `Clip1C`). `p0`/`p1` have stride `ps`.
///
/// The formulas are 8-274 / 8-275 (uni, including the `logWDC == 0` case without shift or
/// rounding), 8-276 (bi) and 8-271..8-273 (default: copy or rounded average, no clip — the
/// average of in-range samples is in range). The offsets `o0`/`o1` arrive **already scaled** by
/// `1 << (BitDepth - 8)`: that scaling belongs to the weight derivation (clause 8.4.3, equations
/// 8-291/8-292 for luma, 8-296/8-297 for chroma), not to this process. `log_wd` is clamped to
/// `0..=30` and the products are computed in `i64`, so hostile weights or denominators cannot
/// overflow or shift out of range (the bitstream constrains them to `0..=7`, `-128..=127` and
/// `-32768..=32767`).
#[allow(clippy::too_many_arguments)]
pub fn weighted_store(dst: &mut [u16], ds: usize, p0: Option<&[u16]>, p1: Option<&[u16]>, ps: usize, bw: usize, bh: usize, w: Weight, max: i32) {
    if bw == 0 || bh == 0 {
        return;
    }
    let rows = |len: usize, stride: usize| (bh - 1).checked_mul(stride).and_then(|v| v.checked_add(bw)).is_some_and(|need| len >= need);
    let have0 = p0.is_none_or(|p| rows(p.len(), ps));
    let have1 = p1.is_none_or(|p| rows(p.len(), ps));
    if !rows(dst.len(), ds) || !have0 || !have1 {
        return;
    }
    match (p0, p1, w) {
        (Some(a), Some(b), Weight::Default) => {
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                let rb = &b[r * ps..r * ps + bw];
                for ((d, &x), &y) in d.iter_mut().zip(ra).zip(rb) {
                    *d = ((x as u32 + y as u32 + 1) >> 1) as u16;
                }
            }
        }
        (Some(a), Some(b), Weight::Weighted { log_wd, w0, w1, o0, o1 }) => {
            let ld = log_wd.clamp(0, 30) as u32;
            let round = 1i64 << ld;
            let sh = ld + 1;
            // 8-276: Clip1( ( ( p0 * w0 + p1 * w1 + 2^logWD ) >> ( logWD + 1 ) ) + ( ( o0 + o1 + 1 ) >> 1 ) )
            let off = (i64::from(o0) + i64::from(o1) + 1) >> 1;
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                let rb = &b[r * ps..r * ps + bw];
                for ((d, &x), &y) in d.iter_mut().zip(ra).zip(rb) {
                    let v = ((i64::from(x) * i64::from(w0) + i64::from(y) * i64::from(w1) + round) >> sh) + off;
                    *d = clip1(v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32, max);
                }
            }
        }
        (Some(a), None, w) | (None, Some(a), w) => {
            let (wt, o, log_wd) = match (w, p0.is_some()) {
                (Weight::Default, _) => {
                    for r in 0..bh {
                        copy_n(&mut dst[r * ds..], &a[r * ps..], bw);
                    }
                    return;
                }
                (Weight::Weighted { w0, o0, log_wd, .. }, true) => (w0, o0, log_wd),
                (Weight::Weighted { w1, o1, log_wd, .. }, false) => (w1, o1, log_wd),
            };
            let ld = log_wd.clamp(0, 30) as u32;
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                for (d, &x) in d.iter_mut().zip(ra) {
                    // 8-274/8-275: with logWDC >= 1 round by 2^(logWDC-1) and shift, else no shift.
                    let v = if ld >= 1 {
                        ((i64::from(x) * i64::from(wt) + (1i64 << (ld - 1))) >> ld) + i64::from(o)
                    } else {
                        i64::from(x) * i64::from(wt) + i64::from(o)
                    };
                    *d = clip1(v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32, max);
                }
            }
        }
        (None, None, _) => {}
    }
}

#[cfg(test)]
mod reference {
    use super::*;

    /// Luma sample interpolation for a bw x bh block. (x, y) is the integer sample position
    /// (block position + (mv >> 2)); (fx, fy) the quarter-sample fraction. Output stride is `os`.
    #[allow(clippy::too_many_arguments)]
    pub fn mc_luma_ref(p: PlaneRef, x: i32, y: i32, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u16], os: usize, max: i32) {
        let mut win = [0u16; WIN * WIN];
        fetch_window(p, x, y, bw, bh, &mut win);
        // sample G at (r, c) in block coordinates is win[(r + 2) * WIN + c + 2]
        let g = |r: usize, c: usize| win[(r + 2) * WIN + c + 2] as i32;
        // unclipped horizontal half-sample b1 at block row r (-2..bh+3 via offset) and column c
        let b1 = |r: isize, c: usize| {
            let base = ((r + 2) as usize) * WIN + c;
            tap6(win[base] as i32, win[base + 1] as i32, win[base + 2] as i32, win[base + 3] as i32, win[base + 4] as i32, win[base + 5] as i32)
        };
        let h1 = |r: usize, c: isize| {
            let col = (c + 2) as usize;
            tap6(
                win[r * WIN + col] as i32,
                win[(r + 1) * WIN + col] as i32,
                win[(r + 2) * WIN + col] as i32,
                win[(r + 3) * WIN + col] as i32,
                win[(r + 4) * WIN + col] as i32,
                win[(r + 5) * WIN + col] as i32,
            )
        };
        match (fx, fy) {
            (0, 0) => {
                for r in 0..bh {
                    out[r * os..r * os + bw].copy_from_slice(&win[(r + 2) * WIN + 2..(r + 2) * WIN + 2 + bw]);
                }
            }
            (_, 0) => {
                // a, b, c
                for r in 0..bh {
                    for c in 0..bw {
                        let b = clip1((b1(r as isize, c) + 16) >> 5, max) as i32;
                        out[r * os + c] = match fx {
                            1 => clip1((g(r, c) + b + 1) >> 1, max),
                            2 => b as u16,
                            _ => clip1((g(r, c + 1) + b + 1) >> 1, max),
                        };
                    }
                }
            }
            (0, _) => {
                // d, h, n
                for r in 0..bh {
                    for c in 0..bw {
                        let h = clip1((h1(r, c as isize) + 16) >> 5, max) as i32;
                        out[r * os + c] = match fy {
                            1 => clip1((g(r, c) + h + 1) >> 1, max),
                            2 => h as u16,
                            _ => clip1((g(r + 1, c) + h + 1) >> 1, max),
                        };
                    }
                }
            }
            (2, _) | (_, 2) => {
                // j-based: j needs b1 for rows -2..bh+3
                let mut bcol = [0i32; WIN * WIN];
                for r in 0..bh + 5 {
                    for c in 0..bw {
                        bcol[r * WIN + c] = b1(r as isize - 2, c);
                    }
                }
                for r in 0..bh {
                    for c in 0..bw {
                        let j1 = tap6(
                            bcol[r * WIN + c],
                            bcol[(r + 1) * WIN + c],
                            bcol[(r + 2) * WIN + c],
                            bcol[(r + 3) * WIN + c],
                            bcol[(r + 4) * WIN + c],
                            bcol[(r + 5) * WIN + c],
                        );
                        let j = clip1((j1 + 512) >> 10, max) as i32;
                        let v = match (fx, fy) {
                            (2, 2) => j,
                            (2, 1) => (clip1((bcol[(r + 2) * WIN + c] + 16) >> 5, max) as i32 + j + 1) >> 1, // f
                            (2, 3) => (clip1((bcol[(r + 3) * WIN + c] + 16) >> 5, max) as i32 + j + 1) >> 1, // q
                            (1, 2) => (clip1((h1(r, c as isize) + 16) >> 5, max) as i32 + j + 1) >> 1,       // i
                            _ => (clip1((h1(r, c as isize + 1) + 16) >> 5, max) as i32 + j + 1) >> 1,        // k
                        };
                        out[r * os + c] = clip1(v, max);
                    }
                }
            }
            _ => {
                // e, g, p, r: average of a horizontal half-sample (b or s) and a vertical one (h or m)
                for r in 0..bh {
                    for c in 0..bw {
                        let hr = if fy == 1 { r as isize } else { r as isize + 1 };
                        let vc = if fx == 1 { c as isize } else { c as isize + 1 };
                        let bh_ = clip1((b1(hr, c) + 16) >> 5, max) as i32;
                        let vv = clip1((h1(r, vc) + 16) >> 5, max) as i32;
                        out[r * os + c] = clip1((bh_ + vv + 1) >> 1, max);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(w: usize, h: usize, f: impl Fn(usize, usize) -> u16) -> Vec<u16> {
        (0..w * h).map(|i| f(i % w, i / w)).collect()
    }

    #[test]
    fn full_sample_copy_and_clamp() {
        let d = plane(16, 16, |x, y| (x + 16 * y) as u16);
        let p = PlaneRef { data: &d, width: 16, height: 16, stride: 16 };
        let mut out = [0u16; 16];
        mc_luma(p, 2, 3, 0, 0, 4, 4, &mut out, 4, 255);
        assert_eq!(&out[..4], &[50, 51, 52, 53]);
        // clamped: far outside to the top-left gives sample (0,0)
        mc_luma(p, -40, -40, 0, 0, 4, 4, &mut out, 4, 255);
        assert!(out.iter().all(|&v| v == 0));
    }

    #[test]
    fn half_sample_constant_is_constant() {
        // 8-bit and 10-bit planes: any fraction keeps a constant field constant
        for &(value, max) in &[(100u16, 255i32), (700u16, 1023i32)] {
            let d = vec![value; 32 * 32];
            let p = PlaneRef { data: &d, width: 32, height: 32, stride: 32 };
            for fx in 0..4 {
                for fy in 0..4 {
                    let mut out = [0u16; 64];
                    mc_luma(p, 8, 8, fx, fy, 8, 8, &mut out, 8, max);
                    assert!(out.iter().all(|&v| v == value), "{fx},{fy}");
                    mc_chroma(p, 8, 8, fx * 2, fy * 2, 8, 8, &mut out, 8, max, false);
                    assert!(out.iter().all(|&v| v == value));
                    mc_chroma(p, 8, 8, fx * 2, fy * 2, 8, 8, &mut out, 8, max, true);
                    assert!(out.iter().all(|&v| v == value));
                }
            }
        }
    }

    #[test]
    fn half_sample_horizontal_ramp() {
        // linear ramp: 6-tap filter of a linear function is exact: b = (G + H) / 2 (rounded)
        let d = plane(32, 32, |x, _| (x * 4) as u16);
        let p = PlaneRef { data: &d, width: 32, height: 32, stride: 32 };
        let mut out = [0u16; 16];
        mc_luma(p, 8, 8, 2, 0, 4, 4, &mut out, 4, 255);
        assert_eq!(&out[..4], &[34, 38, 42, 46]);
        mc_luma(p, 8, 8, 1, 0, 4, 4, &mut out, 4, 255);
        assert_eq!(&out[..4], &[33, 37, 41, 45]);
    }

    #[test]
    fn fast_luma_matches_reference() {
        // 8-bit range at max 255 (same samples as the original u8 test) and 10-bit at max 1023
        for &(shift, mask, max) in &[(24u32, 0xffu32, 255i32), (22, 0x3ff, 1023)] {
            let mut seed = 7u32;
            let d: Vec<u16> = (0..40 * 36)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    ((seed >> shift) & mask) as u16
                })
                .collect();
            let p = PlaneRef { data: &d, width: 40, height: 36, stride: 40 };
            for &(bw, bh) in &[(16, 16), (16, 8), (8, 16), (8, 8), (8, 4), (4, 8), (4, 4)] {
                for fx in 0..4 {
                    for fy in 0..4 {
                        for &(x, y) in &[(10, 9), (-3, -5), (30, 30), (0, 0), (2, 2), (38, 1)] {
                            let mut a = [0u16; 256];
                            let mut b = [0u16; 256];
                            mc_luma(p, x, y, fx, fy, bw, bh, &mut a, 16, max);
                            reference::mc_luma_ref(p, x, y, fx, fy, bw, bh, &mut b, 16, max);
                            assert_eq!(a, b, "max {max} {bw}x{bh} frac {fx},{fy} at {x},{y}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn chroma422_vertical_fraction_is_even_eighths() {
        // vertical step: row 0 = 0, row 1 = 64. For a 1-column block at (0, 0) with fx = 0,
        // 8-270 gives (8 * yFracC * 64 + 32) >> 6 = 8 * yFracC for the derived yFracC
        // (the exact eighth-way interpolation of the 0 -> 64 step):
        let step = plane(8, 2, |_, y| if y == 0 { 0 } else { 64 });
        let p = PlaneRef { data: &step, width: 8, height: 2, stride: 8 };
        let mut out = [0u16; 1];
        // 4:2:2: yFracC = (fy & 3) << 1 -> raw fy 0,1,2,3 give yFracC 0,2,4,6 and outputs 0,16,32,48
        for &(fy, want) in &[(0u32, 0u16), (1, 16), (2, 32), (3, 48), (5, 16), (7, 48)] {
            mc_chroma(p, 0, 0, 0, fy, 1, 1, &mut out, 1, 1023, true);
            assert_eq!(out[0], want, "422 raw fy {fy}");
        }
        // 4:2:0: yFracC = fy verbatim — 422 raw fy 1 behaves like 420 fy 2, 422 raw 3 like 420 6
        mc_chroma(p, 0, 0, 0, 2, 1, 1, &mut out, 1, 1023, false);
        assert_eq!(out[0], 16);
        mc_chroma(p, 0, 0, 0, 6, 1, 1, &mut out, 1, 1023, false);
        assert_eq!(out[0], 48);
        // horizontal fraction is the raw bits in both modes: xFracC = fx -> (8 * 3 * 64 + 32) >> 6 = 24
        let steph = plane(2, 2, |x, _| if x == 0 { 0 } else { 64 });
        let ph = PlaneRef { data: &steph, width: 2, height: 2, stride: 2 };
        for &c422 in &[false, true] {
            mc_chroma(ph, 0, 0, 3, 0, 1, 1, &mut out, 1, 1023, c422);
            assert_eq!(out[0], 24, "chroma422 {c422}");
        }
    }

    #[test]
    fn chroma_frac422_derivation() {
        // 8-231: xIntC = xAL / 2 + (mvCLX[0] >> 3); 8-232: yIntC = yAL + (mvCLX[1] >> 2)
        // 8-233: fx = mv & 7; 8-234: fy raw bits (the kernel shifts them)
        assert_eq!(chroma_frac422(16, 32, [9, 5]), (9, 33, 1, 5));
        // arithmetic shifts keep the floor for negative vectors; & 7 is two's-complement
        assert_eq!(chroma_frac422(4, 4, [-1, -1]), (1, 3, 7, 7));
        // whole-sample motion: no fractions
        assert_eq!(chroma_frac422(0, 0, [16, 8]), (2, 2, 0, 0));
    }

    #[test]
    fn weighting() {
        let a = [100u16; 4];
        let b = [50u16; 4];
        let mut d = [0u16; 4];
        weighted_store(&mut d, 4, Some(&a), Some(&b), 4, 4, 1, Weight::Default, 255);
        assert_eq!(d, [75; 4]);
        // 8-274 with logWDC = 5: ((100 * 16 + 16) >> 5) + 3
        weighted_store(&mut d, 4, Some(&a), None, 4, 4, 1, Weight::Weighted { log_wd: 5, w0: 16, w1: 0, o0: 3, o1: 0 }, 255);
        assert_eq!(d, [53; 4]);
    }

    #[test]
    fn weighted_10bit_formulas_and_clip() {
        let mut d = [0u16; 1];
        // 8-274 "else" (logWDC == 0): no shift, no rounding
        let a = [1000u16];
        weighted_store(&mut d, 1, Some(&a), None, 1, 1, 1, Weight::Weighted { log_wd: 0, w0: 1, w1: 0, o0: 5, o1: 0 }, 1023);
        assert_eq!(d, [1005]);
        // 8-274 with the offset pre-scaled by 1 << (BitDepthY - 8) on the slice side: 1100 -> Clip1Y
        weighted_store(&mut d, 1, Some(&a), None, 1, 1, 1, Weight::Weighted { log_wd: 1, w0: 2, w1: 0, o0: 100, o1: 0 }, 1023);
        assert_eq!(d, [1023]);
        // 8-276: ((100*16 + 50*16 + 32) >> 6) + ((12 + 4 + 1) >> 1) = 38 + 8
        let (a, b) = ([100u16], [50u16]);
        weighted_store(&mut d, 1, Some(&a), Some(&b), 1, 1, 1, Weight::Weighted { log_wd: 5, w0: 16, w1: 16, o0: 12, o1: 4 }, 1023);
        assert_eq!(d, [46]);
        // 8-273 at 10-bit: rounded average, no clip
        weighted_store(&mut d, 1, Some(&a), Some(&b), 1, 1, 1, Weight::Default, 1023);
        assert_eq!(d, [75]);
        // negative result clips to 0: ((10 * -128 + 64) >> 7) - 100 = -110
        weighted_store(&mut d, 1, Some(&a), None, 1, 1, 1, Weight::Weighted { log_wd: 7, w0: -128, w1: 0, o0: -100, o1: 0 }, 1023);
        assert_eq!(d, [0]);
    }

    #[test]
    fn hostile_weights_sizes_and_buffers_do_not_panic() {
        let a = [1000u16; 4];
        let b = [500u16; 4];
        let mut d = [7u16; 4];
        // impossible log denominators and extreme weights/offsets: clamped math, in-range stores
        for &w in &[
            Weight::Weighted { log_wd: 999, w0: i32::MAX, w1: i32::MIN, o0: i32::MAX, o1: i32::MIN },
            Weight::Weighted { log_wd: -7, w0: i32::MIN, w1: i32::MAX, o0: i32::MIN, o1: i32::MAX },
        ] {
            weighted_store(&mut d, 4, Some(&a), Some(&b), 4, 4, 1, w, 1023);
            weighted_store(&mut d, 4, Some(&a), None, 4, 4, 1, w, 1023);
            assert!(d.iter().all(|&v| v <= 1023));
        }
        // too-small output and window: the functions write nothing rather than panicking
        let mut small = [7u16; 2];
        weighted_store(&mut small, 4, Some(&a), None, 4, 4, 1, Weight::Default, 255);
        assert_eq!(small, [7; 2]);
        let mut out = [7u16; 4];
        mc_luma_win(&a, 4, 1, 1, 4, 4, &mut out, 4, 255);
        assert_eq!(out, [7; 4]);
        mc_chroma_win(&a, 4, 3, 3, 4, 4, &mut out, 4, 255, true);
        assert_eq!(out, [7; 4]);
        // zero / oversized blocks
        mc_luma_win(&a, 4, 0, 0, 0, 0, &mut out, 4, 255);
        mc_chroma_win(&a, 4, 0, 0, 32, 32, &mut out, 4, 255, false);
        assert_eq!(out, [7; 4]);
    }
}
