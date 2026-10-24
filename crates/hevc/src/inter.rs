//! Fractional sample interpolation (8.5.3.3.3) and weighted sample prediction (8.5.3.3.4).

use crate::picture::{Frame, Sample};
use crate::spec_tables::{CHROMA_FILTER, LUMA_FILTER};

/// Max prediction block size + filter margin.
const WIN: usize = 64 + 7;

/// Reference windows and first-pass rows, reused between blocks (no per-block zeroing).
pub struct McScratch {
    win: Vec<i16>,
    tmp: Vec<i16>,
}

impl Default for McScratch {
    fn default() -> Self {
        McScratch { win: vec![0; WIN * WIN], tmp: vec![0; WIN * WIN] }
    }
}

/// A sample the interpolation filters read: reference samples (`u8` / `u16`) or 16-bit window
/// and first-pass values.
trait Px: Copy {
    fn w32(self) -> i32;
    fn w16(self) -> i16;
}

impl Px for i16 {
    #[inline(always)]
    fn w32(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn w16(self) -> i16 {
        self
    }
}

impl<S: Sample> Px for S {
    #[inline(always)]
    fn w32(self) -> i32 {
        self.to_i32()
    }
    #[inline(always)]
    fn w16(self) -> i16 {
        self.to_i16()
    }
}

/// `out[j] = (sum_k f[k] * tap(k)[j]) >> shift` for `W` outputs: taps outer, outputs inner, so
/// each tap is one vector multiply-add across the row (exact integer sums). `tap(k)` starts at
/// the samples under filter tap k: the row shifted by k (horizontal) or the k-th row (vertical);
/// a function rather than an array of slices so nothing goes through the stack.
#[inline(always)]
fn filt_w<'a, P: Px + 'a, const T: usize, const W: usize>(tap: impl Fn(usize) -> Option<&'a [P]>, f: &[i8; T], shift: u32, out: &mut [i16]) {
    let mut acc = [0i32; W];
    for k in 0..T {
        let c = f[k] as i32;
        let Some(p) = tap(k).and_then(|s| s.first_chunk::<W>()) else {
            return;
        };
        for j in 0..W {
            acc[j] += c * p[j].w32();
        }
    }
    let Some(o) = out.first_chunk_mut::<W>() else {
        return;
    };
    for j in 0..W {
        o[j] = (acc[j] >> shift) as i16;
    }
}

/// [`filt_w`] in 16-bit lanes, for a first pass over 8-bit samples (`shift` = 0): the filtered
/// value is then at most 88 * 255 and at least -24 * 255 (the positive / negative taps of any
/// luma or chroma filter, 8.5.3.3.3.1 / 8.5.3.3.3.2), so it fits in an `i16`, and wrapping sums
/// are exact whenever the final value fits. Twice the lanes of [`filt_w`] per vector.
#[inline(always)]
fn filt16_w<'a, P: Px + 'a, const T: usize, const W: usize>(tap: impl Fn(usize) -> Option<&'a [P]>, f: &[i8; T], out: &mut [i16]) {
    let mut acc = [0i16; W];
    for k in 0..T {
        let c = f[k] as i16;
        let Some(p) = tap(k).and_then(|s| s.first_chunk::<W>()) else {
            return;
        };
        for j in 0..W {
            acc[j] = acc[j].wrapping_add(c.wrapping_mul(p[j].w16()));
        }
    }
    if let Some(o) = out.first_chunk_mut::<W>() {
        *o = acc;
    }
}

/// [`filt_w`] for a run of `w` outputs (prediction block widths are fixed sizes); in 16-bit lanes
/// ([`filt16_w`]) when `narrow` (a first pass over 8-bit samples).
#[inline(always)]
fn filt<'a, P: Px + 'a, const T: usize>(tap: impl Fn(usize) -> Option<&'a [P]> + Copy, f: &[i8; T], shift: u32, out: &mut [i16], w: usize, narrow: bool) {
    if narrow {
        match w {
            4 => return filt16_w::<P, T, 4>(tap, f, out),
            8 => return filt16_w::<P, T, 8>(tap, f, out),
            12 => return filt16_w::<P, T, 12>(tap, f, out),
            16 => return filt16_w::<P, T, 16>(tap, f, out),
            24 => return filt16_w::<P, T, 24>(tap, f, out),
            32 => return filt16_w::<P, T, 32>(tap, f, out),
            48 => return filt16_w::<P, T, 48>(tap, f, out),
            64 => return filt16_w::<P, T, 64>(tap, f, out),
            _ => {}
        }
    }
    match w {
        2 => filt_w::<P, T, 2>(tap, f, shift, out),
        4 => filt_w::<P, T, 4>(tap, f, shift, out),
        6 => filt_w::<P, T, 6>(tap, f, shift, out),
        8 => filt_w::<P, T, 8>(tap, f, shift, out),
        12 => filt_w::<P, T, 12>(tap, f, shift, out),
        16 => filt_w::<P, T, 16>(tap, f, shift, out),
        24 => filt_w::<P, T, 24>(tap, f, shift, out),
        32 => filt_w::<P, T, 32>(tap, f, shift, out),
        48 => filt_w::<P, T, 48>(tap, f, shift, out),
        64 => filt_w::<P, T, 64>(tap, f, shift, out),
        _ => {
            for (j, o) in out.iter_mut().take(w).enumerate() {
                let s: i32 = (0..T).map(|k| f[k] as i32 * tap(k).and_then(|s| s.get(j)).map_or(0, |p| p.w32())).sum();
                *o = (s >> shift) as i16;
            }
        }
    }
}

/// The taps of a horizontal filter along `row`: the row shifted by k samples.
#[inline(always)]
fn h_taps<'a, P>(row: &'a [P]) -> impl Fn(usize) -> Option<&'a [P]> + Copy {
    move |k| row.get(k..)
}

/// The taps of a vertical filter: row `r + k` from sample `x` on.
#[inline(always)]
fn v_taps<'a, P>(rows: &'a [&'a [P]], r: usize, x: usize) -> impl Fn(usize) -> Option<&'a [P]> + Copy {
    move |k| rows.get(r + k).and_then(|row| row.get(x..))
}

/// Separable sub-sample interpolation of a w x h block from the window `rows` (h + T - 1 rows of
/// at least w + T - 1 samples; the block's top-left sample at (m, m) for a T-tap filter with
/// m = T / 2 - 1). The window rows are the reference picture's own lines when the window lies
/// inside it horizontally, or lines of an edge-replicated copy.
#[allow(clippy::too_many_arguments)]
fn interpolate<P: Px, const T: usize>(
    rows: &[&[P]],
    tmp: &mut [i16],
    fx: usize,
    fy: usize,
    hf: &[i8; T],
    vf: &[i8; T],
    w: usize,
    h: usize,
    shift1: u32,
    shift3: u32,
    out: &mut [i16],
) {
    let m = T / 2 - 1;
    // 8-bit samples: first passes in 16-bit lanes (see filt16_w)
    let narrow = shift1 == 0 && shift3 == 6;
    let out_rows = out.chunks_mut(w).take(h);
    if fx == 0 && fy == 0 {
        for (r, o) in out_rows.enumerate() {
            let Some(src) = rows.get(r + m).and_then(|row| row.get(m..)) else { return };
            for (o, &s) in o.iter_mut().zip(src) {
                *o = s.w16() << shift3;
            }
        }
    } else if fy == 0 {
        for (r, o) in out_rows.enumerate() {
            let Some(row) = rows.get(r + m) else { return };
            filt(h_taps(row), hf, shift1, o, w, narrow);
        }
    } else if fx == 0 {
        for (r, o) in out_rows.enumerate() {
            filt(v_taps(rows, r, m), vf, shift1, o, w, narrow);
        }
    } else {
        for (row, t) in rows.iter().take(h + T - 1).zip(tmp.chunks_mut(WIN)) {
            filt(h_taps(row), hf, shift1, t, w, narrow);
        }
        let mut trows: [&[i16]; WIN] = [&[]; WIN];
        for (t, row) in trows.iter_mut().zip(tmp.chunks(WIN)).take(h + T - 1) {
            *t = row;
        }
        for (r, o) in out_rows.enumerate() {
            filt(v_taps(&trows, r, 0), vf, 6, o, w, false);
        }
    }
}

/// Prediction samples of a w x h block of plane `c` whose filter window (`T`-tap filters) starts
/// at (x0, y0): read straight from the reference lines when the window is inside the picture
/// horizontally, else from an edge-replicated copy.
#[allow(clippy::too_many_arguments)]
fn predict_block<S: Sample, const T: usize>(
    f: &Frame,
    c: usize,
    (x0, y0): (i32, i32),
    (fx, fy): (usize, usize),
    filters: (&[i8; T], &[i8; T]),
    (w, h): (usize, usize),
    (shift1, shift3): (u32, u32),
    out: &mut [i16],
    sc: &mut McScratch,
) {
    let (ww, wh) = (w + T - 1, h + T - 1);
    let McScratch { win, tmp } = sc;
    let mut rows: [&[S]; WIN] = [&[]; WIN];
    if let Some(rows) = rows.get_mut(..wh)
        && f.window_rows(c, x0, y0, ww, rows)
    {
        interpolate::<S, T>(rows, tmp, fx, fy, filters.0, filters.1, w, h, shift1, shift3, out);
        return;
    }
    if c == 0 {
        f.luma_window::<S>(x0, y0, ww, wh, win, WIN);
    } else {
        f.chroma_window::<S>(c - 1, x0, y0, ww, wh, win, WIN);
    }
    let mut wrows: [&[i16]; WIN] = [&[]; WIN];
    for (r, row) in wrows.iter_mut().zip(win.chunks(WIN)).take(wh) {
        *r = row;
    }
    interpolate::<i16, T>(&wrows[..wh.min(WIN)], tmp, fx, fy, filters.0, filters.1, w, h, shift1, shift3, out);
}

/// Luma prediction samples (14-bit intermediate precision) of a w x h block at (x, y) displaced by the
/// quarter-sample motion vector `mv`, into `out` (stride w).
#[allow(clippy::too_many_arguments)]
pub fn mc_luma<S: Sample>(f: &Frame, x: i32, y: i32, mv: [i16; 2], w: usize, h: usize, out: &mut [i16], sc: &mut McScratch) {
    let bd = f.bit_depth;
    let (fx, fy) = ((mv[0] & 3) as usize, (mv[1] & 3) as usize);
    let xi = x + (mv[0] as i32 >> 2);
    let yi = y + (mv[1] as i32 >> 2);
    let shift1 = bd.min(12) - 8;
    let shift3 = 14 - bd;
    predict_block::<S, 8>(f, 0, (xi - 3, yi - 3), (fx, fy), (&LUMA_FILTER[fx], &LUMA_FILTER[fy]), (w, h), (shift1, shift3), out, sc);
}

/// Chroma prediction samples of a w x h chroma block at chroma position (x, y) with a 1/8-sample
/// vector (4:2:0).
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma<S: Sample>(f: &Frame, c: usize, x: i32, y: i32, mv: [i16; 2], w: usize, h: usize, out: &mut [i16], sc: &mut McScratch) {
    let bd = f.bit_depth_c;
    let (fx, fy) = ((mv[0] & 7) as usize, (mv[1] & 7) as usize);
    let xi = x + (mv[0] as i32 >> 3);
    let yi = y + (mv[1] as i32 >> 3);
    let shift1 = bd.min(12) - 8;
    let shift3 = 14 - bd;
    predict_block::<S, 4>(f, c + 1, (xi - 1, yi - 1), (fx, fy), (&CHROMA_FILTER[fx], &CHROMA_FILTER[fy]), (w, h), (shift1, shift3), out, sc);
}

/// Calls `$f::<_, W>(args)` with `W` the prediction block width `$w` as a constant (block widths are
/// fixed sizes; a constant width lets the row loops vectorise even for 4- and 8-sample rows), or
/// `$f::<0>` for any other width.
macro_rules! by_width {
    ($w:expr, $f:ident($($a:expr),*)) => {
        match $w {
            2 => $f::<_, 2>($($a),*),
            4 => $f::<_, 4>($($a),*),
            6 => $f::<_, 6>($($a),*),
            8 => $f::<_, 8>($($a),*),
            12 => $f::<_, 12>($($a),*),
            16 => $f::<_, 16>($($a),*),
            24 => $f::<_, 24>($($a),*),
            32 => $f::<_, 32>($($a),*),
            48 => $f::<_, 48>($($a),*),
            64 => $f::<_, 64>($($a),*),
            _ => $f::<_, 0>($($a),*),
        }
    };
}

/// Default weighted prediction, one list (8-262).
pub fn put_uni<S: Sample>(p: &[i16], w: usize, h: usize, bd: u32, dst: &mut [S], ds: usize) {
    by_width!(w, put_uni_w(p, w, h, bd, dst, ds))
}

/// [`put_uni`] for rows of `W` samples (`W` = 0: rows of `w`).
///
/// In 16-bit lanes: prediction samples are 14-bit values (8.5.3.3.3: at most about +/-27000 for any
/// bit depth, as the reference samples are within `0..2^bd`), so `s + off` and the clipped result fit
/// in an `i16` and the sums are exact. SSE2 clips 16-bit lanes in one instruction each, 32-bit lanes
/// in four.
#[inline(always)]
fn put_uni_w<S: Sample, const W: usize>(p: &[i16], w: usize, h: usize, bd: u32, dst: &mut [S], ds: usize) {
    let w = if W > 0 { W } else { w };
    let shift = 14 - bd;
    let off: i16 = if shift > 0 { 1 << (shift - 1) } else { 0 };
    let max = ((1i32 << bd) - 1) as i16;
    // Rows as slices and min / max clipping (`clamp` asserts its bounds) so the loop vectorises.
    for (d, s) in dst.chunks_mut(ds).zip(p[..w * h].chunks(w)) {
        for (d, &s) in d[..w].iter_mut().zip(s) {
            *d = S::from_i32((s.wrapping_add(off) >> shift).max(0).min(max) as i32);
        }
    }
}

/// Default weighted prediction, bi-prediction (8-263).
pub fn put_bi<S: Sample>(p0: &[i16], p1: &[i16], w: usize, h: usize, bd: u32, dst: &mut [S], ds: usize) {
    by_width!(w, put_bi_w(p0, p1, w, h, bd, dst, ds))
}

/// [`put_bi`] for rows of `W` samples (`W` = 0: rows of `w`).
///
/// In 16-bit lanes, exactly: with `off = 2^(shift - 1)` even, `(a + b + off) >> shift` equals
/// `((a >> 1) + (b >> 1) + (a & b & 1) + off / 2) >> (shift - 1)` (the two dropped low bits only
/// carry when both are set), and `(a >> 1) + (b >> 1)` fits in an `i16` (see [`put_uni_w`]).
#[inline(always)]
fn put_bi_w<S: Sample, const W: usize>(p0: &[i16], p1: &[i16], w: usize, h: usize, bd: u32, dst: &mut [S], ds: usize) {
    let w = if W > 0 { W } else { w };
    let shift = 15 - bd; // >= 3 (bd <= 12)
    let half_off: i16 = 1 << (shift - 2);
    let max = ((1i32 << bd) - 1) as i16;
    for ((d, a), b) in dst.chunks_mut(ds).zip(p0[..w * h].chunks(w)).zip(p1[..w * h].chunks(w)) {
        for ((d, &a), &b) in d[..w].iter_mut().zip(a).zip(b) {
            let v = (a >> 1).wrapping_add(b >> 1).wrapping_add(a & b & 1).wrapping_add(half_off) >> (shift - 1);
            *d = S::from_i32(v.max(0).min(max) as i32);
        }
    }
}

/// Explicit weighted prediction, one list (8-265). `o` is the offset already scaled to the bit depth.
pub fn put_weighted_uni<S: Sample>(p: &[i16], w: usize, h: usize, bd: u32, log2wd: u32, wt: i32, o: i32, dst: &mut [S], ds: usize) {
    let max = (1i32 << bd) - 1;
    for r in 0..h {
        for c in 0..w {
            let v = p[r * w + c] as i32 * wt;
            let v = if log2wd >= 1 { ((v + (1 << (log2wd - 1))) >> log2wd) + o } else { v + o };
            dst[r * ds + c] = S::from_i32(v.max(0).min(max));
        }
    }
}

/// Explicit weighted bi-prediction (8-266).
pub fn put_weighted_bi<S: Sample>(
    p0: &[i16],
    p1: &[i16],
    w: usize,
    h: usize,
    bd: u32,
    log2wd: u32,
    w0: i32,
    w1: i32,
    o0: i32,
    o1: i32,
    dst: &mut [S],
    ds: usize,
) {
    let max = (1i32 << bd) - 1;
    for r in 0..h {
        for c in 0..w {
            let i = r * w + c;
            let v = (p0[i] as i32 * w0 + p1[i] as i32 * w1 + ((o0 + o1 + 1) << log2wd)) >> (log2wd + 1);
            dst[r * ds + c] = S::from_i32(v.max(0).min(max));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            lo + (self.next() % (hi - lo + 1) as u64) as i32
        }
    }

    const WIDTHS: [usize; 11] = [2, 4, 6, 8, 12, 16, 24, 32, 48, 64, 3];

    /// The 16-bit-lane `put_uni` / `put_bi` give exactly the 32-bit formulas of 8-262 / 8-263 over
    /// the whole range of 14-bit prediction samples, for every block width and bit depth.
    #[test]
    fn default_weighted_prediction_matches_the_32_bit_formulas() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for bd in [8u32, 10, 12] {
            let max = (1i32 << bd) - 1;
            for &w in &WIDTHS {
                let h = 3;
                let sample = |rng: &mut Rng| -> i16 {
                    match rng.next() % 6 {
                        0 => -10_240,
                        1 => 26_624,
                        2 => rng.range(-1, 1) as i16,
                        _ => rng.range(-10_240, 26_624) as i16,
                    }
                };
                let p0: Vec<i16> = (0..w * h).map(|_| sample(&mut rng)).collect();
                let p1: Vec<i16> = (0..w * h).map(|_| sample(&mut rng)).collect();
                let ds = w + 5;
                let (mut uni, mut bi) = (vec![0u16; ds * h], vec![0u16; ds * h]);
                put_uni(&p0, w, h, bd, &mut uni, ds);
                put_bi(&p0, &p1, w, h, bd, &mut bi, ds);
                for r in 0..h {
                    for c in 0..w {
                        let (a, b) = (p0[r * w + c] as i32, p1[r * w + c] as i32);
                        let s1 = 14 - bd;
                        let o1 = if s1 > 0 { 1 << (s1 - 1) } else { 0 };
                        assert_eq!(uni[r * ds + c] as i32, ((a + o1) >> s1).clamp(0, max), "uni bd {bd} w {w} a {a}");
                        let s2 = 15 - bd;
                        assert_eq!(bi[r * ds + c] as i32, ((a + b + (1 << (s2 - 1))) >> s2).clamp(0, max), "bi bd {bd} w {w} a {a} b {b}");
                    }
                }
            }
        }
    }

    /// Explicit weighted prediction gives exactly 8-265 / 8-266 for every block width, bit depth,
    /// denominator and the extreme weights and offsets, and with the default weights (2^denom,
    /// offset 0) exactly the default weighted prediction (8-262 / 8-263) that replaces it then.
    #[test]
    fn explicit_weighted_prediction_matches_the_formulas() {
        let mut rng = Rng(0x7f4a_7c15_9e37_79b9);
        for bd in [8u32, 10, 12] {
            let max = (1i32 << bd) - 1;
            for &w in &WIDTHS {
                for _ in 0..40 {
                    let h = 2;
                    let p0: Vec<i16> = (0..w * h).map(|_| rng.range(-10_240, 26_624) as i16).collect();
                    let p1: Vec<i16> = (0..w * h).map(|_| rng.range(-10_240, 26_624) as i16).collect();
                    let log2wd = rng.range(0, 7) as u32 + 14 - bd;
                    let wt = |rng: &mut Rng| [-128, 255, rng.range(-128, 255)][rng.range(0, 2) as usize];
                    let (w0, w1) = (wt(&mut rng), wt(&mut rng));
                    let off = |rng: &mut Rng| [-128, 127, rng.range(-128, 127)][rng.range(0, 2) as usize] << (bd - 8);
                    let (o0, o1) = (off(&mut rng), off(&mut rng));
                    let ds = w + 3;
                    let (mut uni, mut bi) = (vec![0u16; ds * h], vec![0u16; ds * h]);
                    put_weighted_uni(&p0, w, h, bd, log2wd, w0, o0, &mut uni, ds);
                    put_weighted_bi(&p0, &p1, w, h, bd, log2wd, w0, w1, o0, o1, &mut bi, ds);
                    let denom = log2wd - (14 - bd);
                    let (mut duni, mut dbi) = (vec![0u16; ds * h], vec![0u16; ds * h]);
                    put_weighted_uni(&p0, w, h, bd, log2wd, 1 << denom, 0, &mut duni, ds);
                    put_weighted_bi(&p0, &p1, w, h, bd, log2wd, 1 << denom, 1 << denom, 0, 0, &mut dbi, ds);
                    let (mut uni0, mut bi0) = (vec![0u16; ds * h], vec![0u16; ds * h]);
                    put_uni(&p0, w, h, bd, &mut uni0, ds);
                    put_bi(&p0, &p1, w, h, bd, &mut bi0, ds);
                    assert_eq!((duni, dbi), (uni0, bi0), "default weights bd {bd} w {w} denom {denom}");
                    for r in 0..h {
                        for c in 0..w {
                            let (a, b) = (p0[r * w + c] as i32, p1[r * w + c] as i32);
                            let v = if log2wd >= 1 { ((a * w0 + (1 << (log2wd - 1))) >> log2wd) + o0 } else { a * w0 + o0 };
                            assert_eq!(uni[r * ds + c] as i32, v.clamp(0, max), "uni bd {bd} w {w}");
                            let v = (a * w0 + b * w1 + ((o0 + o1 + 1) << log2wd)) >> (log2wd + 1);
                            assert_eq!(bi[r * ds + c] as i32, v.clamp(0, max), "bi bd {bd} w {w}");
                        }
                    }
                }
            }
        }
    }

    /// First passes over 8-bit samples in 16-bit lanes equal the 32-bit sums, including the
    /// extreme sample patterns of every filter.
    #[test]
    fn narrow_first_pass_matches_the_32_bit_filter() {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        fn check<const T: usize>(f: &[i8; T], src: &[i16], step: usize, w: usize) {
            let (mut a, mut b) = ([0i16; 64], [0i16; 64]);
            let taps = |k: usize| src.get(k * step..);
            filt(taps, f, 0, &mut a, w, true);
            filt(taps, f, 0, &mut b, w, false);
            assert_eq!(a[..w], b[..w], "filter {f:?} step {step} w {w}");
        }
        for &w in &WIDTHS {
            for step in [1usize, WIN] {
                let len = w + 7 * step + 8;
                let mut patterns: Vec<Vec<i16>> = (0..20).map(|_| (0..len).map(|_| rng.range(0, 255) as i16).collect()).collect();
                patterns.push(vec![255; len]);
                patterns.push(vec![0; len]);
                for f in LUMA_FILTER.iter() {
                    // 255 under positive taps, 0 under negative ones (maximum), and the reverse
                    for sign in [1, -1] {
                        let mut v = vec![0i16; len];
                        for (k, &c) in f.iter().enumerate() {
                            for j in 0..w {
                                v[j + k * step] = if (c as i32) * sign > 0 { 255 } else { 0 };
                            }
                        }
                        check(f, &v, step, w);
                    }
                    for p in &patterns {
                        check(f, p, step, w);
                    }
                }
                for f in CHROMA_FILTER.iter() {
                    for p in &patterns {
                        check(f, p, step, w);
                    }
                }
            }
        }
    }

    fn frame(rng: &mut Rng, w: usize, h: usize, bd: u32) -> Frame {
        let like = Frame::new(0, 0, w, h, 4, bd, bd);
        let max = (1i32 << bd) - 1;
        let y: Vec<u16> = (0..w * h).map(|_| rng.range(0, max) as u16).collect();
        let c: Vec<u16> = (0..w * h / 4).map(|_| rng.range(0, max) as u16).collect();
        let c2: Vec<u16> = (0..w * h / 4).map(|_| rng.range(0, max) as u16).collect();
        Frame::from_planes(1, 0, &like, &y, &c, &c2)
    }

    /// Interpolation from the reference lines in place gives the same prediction samples as from
    /// an edge-replicated copy of the window, for every fractional position, block size and bit
    /// depth, inside the picture and next to its edges (where in-place reads are used for windows
    /// that cross only the top or bottom edge).
    #[test]
    fn in_place_window_matches_the_copied_window() {
        fn check<S: Sample>(rng: &mut Rng, bd: u32) {
            let mut sc = McScratch::default();
            let f = frame(rng, 96, 64, bd);
            let (mut in_place, mut copied) = (0, 0);
            for _ in 0..2000 {
                let c = rng.range(0, 2) as usize;
                let (w, h) = ([4usize, 8, 12, 16, 24, 32][rng.range(0, 5) as usize], [4usize, 8, 16, 32][rng.range(0, 3) as usize]);
                let (w, h) = if c == 0 { (w, h) } else { (w / 2, h / 2) };
                let (x, y) = (rng.range(-12, 100), rng.range(-12, 70));
                let mv = [rng.range(-64, 64) as i16, rng.range(-64, 64) as i16];
                let mut a = vec![0i16; w * h];
                let mut b = vec![0i16; w * h];
                let (t, frac, unit) = if c == 0 { (8usize, 3, 2) } else { (4, 7, 3) };
                let (fx, fy) = ((mv[0] & frac) as usize, (mv[1] & frac) as usize);
                let (x0, y0) = (x + (mv[0] as i32 >> unit) - (t as i32 / 2 - 1), y + (mv[1] as i32 >> unit) - (t as i32 / 2 - 1));
                let (shift1, shift3) = (bd - 8, 14 - bd);
                let mut rows: Vec<&[S]> = vec![&[]; h + t - 1];
                if f.window_rows(c, x0, y0, w + t - 1, &mut rows) {
                    in_place += 1;
                } else {
                    copied += 1;
                }
                // reference: the copied window
                let mut win = vec![0i16; WIN * WIN];
                let mut tmp = vec![0i16; WIN * WIN];
                if c == 0 {
                    f.luma_window::<S>(x0, y0, w + t - 1, h + t - 1, &mut win, WIN);
                } else {
                    f.chroma_window::<S>(c - 1, x0, y0, w + t - 1, h + t - 1, &mut win, WIN);
                }
                let wrows: Vec<&[i16]> = win.chunks(WIN).take(h + t - 1).collect();
                if c == 0 {
                    mc_luma::<S>(&f, x, y, mv, w, h, &mut a, &mut sc);
                    interpolate::<i16, 8>(&wrows, &mut tmp, fx, fy, &LUMA_FILTER[fx], &LUMA_FILTER[fy], w, h, shift1, shift3, &mut b);
                } else {
                    mc_chroma::<S>(&f, c - 1, x, y, mv, w, h, &mut a, &mut sc);
                    interpolate::<i16, 4>(&wrows, &mut tmp, fx, fy, &CHROMA_FILTER[fx], &CHROMA_FILTER[fy], w, h, shift1, shift3, &mut b);
                }
                assert_eq!(a, b, "plane {c} bd {bd} at ({x}, {y}) mv {mv:?} {w}x{h}");
            }
            assert!(in_place > 500 && copied > 100, "{in_place} in place, {copied} copied");
        }
        let mut rng = Rng(0x5851_f42d_4c95_7f2d);
        check::<u8>(&mut rng, 8);
        check::<u16>(&mut rng, 10);
    }

    /// The full-sample uni-prediction shortcut (`Frame::copy_block`) gives what interpolation and
    /// 8-262 give, inside the picture and across every edge (8-bit pictures stored as `u8`, 10-bit
    /// ones as `u16`).
    #[test]
    fn full_sample_copy_matches_interpolation() {
        fn check<S: Sample>(rng: &mut Rng, bd: u32) {
            let mut sc = McScratch::default();
            let f = frame(rng, 48, 40, bd);
            for _ in 0..300 {
                let (w, h) = ([4usize, 8, 12, 16, 24, 32][rng.range(0, 5) as usize], [4usize, 8, 16][rng.range(0, 2) as usize]);
                let (x, y) = (rng.range(-40, 80), rng.range(-40, 70));
                let mut pred = vec![0i16; w * h];
                let (mut a, mut b) = (vec![S::default(); w * h], vec![S::default(); w * h]);
                mc_luma::<S>(&f, 0, 0, [(x * 4) as i16, (y * 4) as i16], w, h, &mut pred, &mut sc);
                put_uni(&pred, w, h, bd, &mut a, w);
                f.copy_block(0, x, y, w, h, &mut b, w);
                assert_eq!(a, b, "luma bd {bd} at ({x}, {y}) {w}x{h}");
                // (the window really was read: not all samples equal the default)
                assert!(a.iter().any(|&v| v != S::default()) || w * h < 8);
                for c in 1..3 {
                    let (cw, ch) = (w / 2, h / 2);
                    mc_chroma::<S>(&f, c - 1, 0, 0, [(x * 8) as i16, (y * 8) as i16], cw, ch, &mut pred, &mut sc);
                    let (mut a, mut b) = (vec![S::default(); cw * ch], vec![S::default(); cw * ch]);
                    put_uni(&pred, cw, ch, bd, &mut a, cw);
                    f.copy_block(c, x, y, cw, ch, &mut b, cw);
                    assert_eq!(a, b, "chroma {c} bd {bd} at ({x}, {y}) {cw}x{ch}");
                }
            }
        }
        let mut rng = Rng(0x0123_4567_89ab_cdef);
        check::<u8>(&mut rng, 8);
        check::<u16>(&mut rng, 10);
    }
}
