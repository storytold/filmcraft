//! Inter prediction sample interpolation (8.4.2.2) and weighted sample prediction (8.4.2.3), for any
//! sample type (`max` is `(1 << bit_depth) - 1`).

use crate::picture::Sample;

/// A reference plane (used by the plane-based helpers in tests).
#[cfg(test)]
#[derive(Clone, Copy)]
pub struct PlaneRef<'a, S: Sample> {
    pub data: &'a [S],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

const WIN: usize = 16 + 5;

/// Copy the (bw+5)x(bh+5) window whose top-left is (x-2, y-2) into `win` (stride WIN), clamping
/// coordinates to the picture.
#[cfg(test)]
#[inline]
fn fetch_window<S: Sample>(p: PlaneRef<S>, x: i32, y: i32, bw: usize, bh: usize, win: &mut [S; WIN * WIN]) {
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

#[inline(always)]
fn clip1<S: Sample>(v: i32, max: i32) -> S {
    S::clip(v, max)
}

/// Copy `n` bytes with a fixed-size move for the common block widths.
#[inline(always)]
fn copy_n<S: Sample>(dst: &mut [S], src: &[S], n: usize) {
    fn fixed<S: Sample, const N: usize>(dst: &mut [S], src: &[S]) {
        if let (Some(d), Some(s)) = (dst.first_chunk_mut::<N>(), src.first_chunk::<N>()) {
            *d = *s;
        }
    }
    match n {
        16 => fixed::<S, 16>(dst, src),
        8 => fixed::<S, 8>(dst, src),
        4 => fixed::<S, 4>(dst, src),
        2 => fixed::<S, 2>(dst, src),
        _ => dst[..n].copy_from_slice(&src[..n]),
    }
}

#[inline(always)]
fn avg<S: Sample>(a: S, b: S) -> S {
    S::from_i32((a.to_i32() + b.to_i32() + 1) >> 1)
}

/// Horizontal 6-tap (unclipped) at every position of `row` for `n` outputs: out[c] uses row[c..c + 6].
#[inline(always)]
fn htap_row<S: Sample>(row: &[S], out: &mut [i32], n: usize) {
    let row = &row[..n + 5];
    for (o, w) in out[..n].iter_mut().zip(row.windows(6)) {
        *o = tap6(w[0].to_i32(), w[1].to_i32(), w[2].to_i32(), w[3].to_i32(), w[4].to_i32(), w[5].to_i32());
    }
}

/// Vertical 6-tap (unclipped) over six rows at columns 0..n.
#[inline(always)]
fn vtap_row<S: Sample>(rows: [&[S]; 6], out: &mut [i32], n: usize) {
    let [a, b, c, d, e, f] = rows.map(|r| &r[..n]);
    for i in 0..n {
        out[i] = tap6(a[i].to_i32(), b[i].to_i32(), c[i].to_i32(), d[i].to_i32(), e[i].to_i32(), f[i].to_i32());
    }
}

/// Luma sample interpolation for a bw x bh block (8.4.2.2.1). (x, y) is the integer sample position
/// (block position + (mv >> 2)); (fx, fy) the quarter-sample fraction. Output stride is `os`.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn mc_luma<S: Sample>(p: PlaneRef<S>, x: i32, y: i32, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [S], os: usize, max: i32) {
    let mut win = [S::default(); WIN * WIN];
    let (x0, y0) = (x - 2, y - 2);
    let inside = x0 >= 0 && y0 >= 0 && x0 as usize + bw + 5 <= p.width && y0 as usize + bh + 5 <= p.height;
    let (src, ss): (&[S], usize) = if inside {
        (&p.data[y0 as usize * p.stride + x0 as usize..], p.stride)
    } else {
        fetch_window(p, x, y, bw, bh, &mut win);
        (&win[..], WIN)
    };
    mc_luma_win(src, ss, fx, fy, bw, bh, out, os, max);
}

/// Luma interpolation from a prepared window: `src[0]` is the sample at (x - 2, y - 2) and `ss` the
/// window stride; the window covers (bw + 5) x (bh + 5) samples.
#[allow(clippy::too_many_arguments)]
pub fn mc_luma_win<S: Sample>(src: &[S], ss: usize, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [S], os: usize, max: i32) {
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
                            o[c] = avg(row[c + 2], clip1((t[c] + 16) >> 5, max));
                        }
                    }
                    2 => {
                        for c in 0..bw {
                            o[c] = clip1((t[c] + 16) >> 5, max);
                        }
                    }
                    _ => {
                        for c in 0..bw {
                            o[c] = avg(row[c + 3], clip1((t[c] + 16) >> 5, max));
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
                        o[c] = avg(g[c], clip1((t[c] + 16) >> 5, max));
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

/// Chroma sample interpolation (8.4.2.2.2) for a bw x bh block at integer position (x, y) with
/// eighth-sample fraction (fx, fy).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma<S: Sample>(p: PlaneRef<S>, x: i32, y: i32, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [S], os: usize) {
    let maxx = p.width as i32 - 1;
    let maxy = p.height as i32 - 1;
    let inside = x >= 0 && y >= 0 && x + bw as i32 <= maxx && y + bh as i32 <= maxy;
    // (bw+1) x (bh+1) source window; copied with edge clamping when it crosses the picture border
    let mut win = [S::default(); 9 * 17];
    let (src, ss): (&[S], usize) = if inside {
        (&p.data[y as usize * p.stride + x as usize..], p.stride)
    } else {
        for r in 0..=bh {
            let yy = (y + r as i32).clamp(0, maxy) as usize;
            for c in 0..=bw {
                win[r * 9 + c] = p.data[yy * p.stride + (x + c as i32).clamp(0, maxx) as usize];
            }
        }
        (&win[..], 9)
    };
    mc_chroma_win(src, ss, fx, fy, bw, bh, out, os);
}

/// Chroma interpolation from a prepared (bw + 1) x (bh + 1) window whose first sample is the integer
/// position of the block.
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma_win<S: Sample>(src: &[S], ss: usize, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [S], os: usize) {
    let (fx, fy) = (fx as i32, fy as i32);
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
            let v = w00 * r0[c].to_i32() + w10 * r0[c + 1].to_i32() + w01 * r1[c].to_i32() + w11 * r1[c + 1].to_i32();
            *o = S::from_i32((v + 32) >> 6);
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

/// Combine prediction(s) into `dst` (stride `ds`). `p0`/`p1` have stride `ps`.
#[allow(clippy::too_many_arguments)]
pub fn weighted_store<S: Sample>(dst: &mut [S], ds: usize, p0: Option<&[S]>, p1: Option<&[S]>, ps: usize, bw: usize, bh: usize, w: Weight, max: i32) {
    match (p0, p1, w) {
        (Some(a), Some(b), Weight::Default) => {
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                let rb = &b[r * ps..r * ps + bw];
                for ((d, &x), &y) in d.iter_mut().zip(ra).zip(rb) {
                    *d = S::from_i32((x.to_i32() + y.to_i32() + 1) >> 1);
                }
            }
        }
        (Some(a), Some(b), Weight::Weighted { log_wd, w0, w1, o0, o1 }) => {
            let round = 1 << log_wd;
            let off = (o0 + o1 + 1) >> 1;
            let sh = log_wd + 1;
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                let rb = &b[r * ps..r * ps + bw];
                for ((d, &x), &y) in d.iter_mut().zip(ra).zip(rb) {
                    *d = clip1(((x.to_i32() * w0 + y.to_i32() * w1 + round) >> sh) + off, max);
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
            let round = if log_wd >= 1 { 1 << (log_wd - 1) } else { 0 };
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                for (d, &x) in d.iter_mut().zip(ra) {
                    *d = clip1(((x.to_i32() * wt + round) >> log_wd) + o, max);
                }
            }
        }
        (None, None, _) => {}
    }
}

#[cfg(test)]
mod reference {
    use super::*;

    fn clip1(v: i32, max: i32) -> i32 {
        v.clamp(0, max)
    }

    /// Luma sample interpolation for a bw x bh block. (x, y) is the integer sample position
    /// (block position + (mv >> 2)); (fx, fy) the quarter-sample fraction. Output stride is `os`.
    #[allow(clippy::too_many_arguments)]
    pub fn mc_luma_ref<S: Sample>(p: PlaneRef<S>, x: i32, y: i32, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [S], os: usize, max: i32) {
        let mut win = [S::default(); WIN * WIN];
        fetch_window(p, x, y, bw, bh, &mut win);
        // sample G at (r, c) in block coordinates is win[(r + 2) * WIN + c + 2]
        let g = |r: usize, c: usize| win[(r + 2) * WIN + c + 2].to_i32();
        // unclipped horizontal half-sample b1 at block row r (-2..bh+3 via offset) and column c
        let b1 = |r: isize, c: usize| {
            let base = ((r + 2) as usize) * WIN + c;
            tap6(win[base].to_i32(), win[base + 1].to_i32(), win[base + 2].to_i32(), win[base + 3].to_i32(), win[base + 4].to_i32(), win[base + 5].to_i32())
        };
        let h1 = |r: usize, c: isize| {
            let col = (c + 2) as usize;
            tap6(
                win[r * WIN + col].to_i32(),
                win[(r + 1) * WIN + col].to_i32(),
                win[(r + 2) * WIN + col].to_i32(),
                win[(r + 3) * WIN + col].to_i32(),
                win[(r + 4) * WIN + col].to_i32(),
                win[(r + 5) * WIN + col].to_i32(),
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
                        let b = clip1((b1(r as isize, c) + 16) >> 5, max);
                        out[r * os + c] = match fx {
                            1 => S::from_i32((g(r, c) + b + 1) >> 1),
                            2 => S::from_i32(b),
                            _ => S::from_i32((g(r, c + 1) + b + 1) >> 1),
                        };
                    }
                }
            }
            (0, _) => {
                // d, h, n
                for r in 0..bh {
                    for c in 0..bw {
                        let h = clip1((h1(r, c as isize) + 16) >> 5, max);
                        out[r * os + c] = match fy {
                            1 => S::from_i32((g(r, c) + h + 1) >> 1),
                            2 => S::from_i32(h),
                            _ => S::from_i32((g(r + 1, c) + h + 1) >> 1),
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
                        let j = clip1((j1 + 512) >> 10, max);
                        let v = match (fx, fy) {
                            (2, 2) => j,
                            (2, 1) => (clip1((bcol[(r + 2) * WIN + c] + 16) >> 5, max) + j + 1) >> 1, // f
                            (2, 3) => (clip1((bcol[(r + 3) * WIN + c] + 16) >> 5, max) + j + 1) >> 1, // q
                            (1, 2) => (clip1((h1(r, c as isize) + 16) >> 5, max) + j + 1) >> 1,       // i
                            _ => (clip1((h1(r, c as isize + 1) + 16) >> 5, max) + j + 1) >> 1,        // k
                        };
                        out[r * os + c] = S::from_i32(v);
                    }
                }
            }
            _ => {
                // e, g, p, r: average of a horizontal half-sample (b or s) and a vertical one (h or m)
                for r in 0..bh {
                    for c in 0..bw {
                        let hr = if fy == 1 { r as isize } else { r as isize + 1 };
                        let vc = if fx == 1 { c as isize } else { c as isize + 1 };
                        let bh_ = clip1((b1(hr, c) + 16) >> 5, max);
                        let vv = clip1((h1(r, vc) + 16) >> 5, max);
                        out[r * os + c] = S::from_i32((bh_ + vv + 1) >> 1);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane<S: Sample>(w: usize, h: usize, f: impl Fn(usize, usize) -> i32) -> Vec<S> {
        (0..w * h).map(|i| S::from_i32(f(i % w, i / w))).collect()
    }

    #[test]
    fn full_sample_copy_and_clamp() {
        let d = plane::<u8>(16, 16, |x, y| (x + 16 * y) as i32 & 255);
        let p = PlaneRef { data: &d, width: 16, height: 16, stride: 16 };
        let mut out = [0u8; 16];
        mc_luma(p, 2, 3, 0, 0, 4, 4, &mut out, 4, 255);
        assert_eq!(&out[..4], &[50, 51, 52, 53]);
        // clamped: far outside to the top-left gives sample (0,0)
        mc_luma(p, -40, -40, 0, 0, 4, 4, &mut out, 4, 255);
        assert!(out.iter().all(|&v| v == 0));
    }

    #[test]
    fn half_sample_constant_is_constant() {
        for (v, max) in [(100u16, 255), (700, 1023)] {
            let d = vec![v; 32 * 32];
            let p = PlaneRef { data: &d, width: 32, height: 32, stride: 32 };
            for fx in 0..4 {
                for fy in 0..4 {
                    let mut out = [0u16; 64];
                    mc_luma(p, 8, 8, fx, fy, 8, 8, &mut out, 8, max);
                    assert!(out.iter().all(|&o| o == v), "{fx},{fy}");
                    mc_chroma(p, 8, 8, fx * 2, fy * 2, 8, 8, &mut out, 8);
                    assert!(out.iter().all(|&o| o == v));
                }
            }
        }
    }

    #[test]
    fn half_sample_horizontal_ramp() {
        // linear ramp: 6-tap filter of a linear function is exact: b = (G + H) / 2 (rounded)
        let d = plane::<u8>(32, 32, |x, _| (x * 4) as i32);
        let p = PlaneRef { data: &d, width: 32, height: 32, stride: 32 };
        let mut out = [0u8; 16];
        mc_luma(p, 8, 8, 2, 0, 4, 4, &mut out, 4, 255);
        assert_eq!(&out[..4], &[34, 38, 42, 46]);
        mc_luma(p, 8, 8, 1, 0, 4, 4, &mut out, 4, 255);
        assert_eq!(&out[..4], &[33, 37, 41, 45]);
    }

    /// The fast interpolation equals the straightforward per-sample reference at 8 and 10 bits
    /// (10-bit data reaches the clipping limits of the 6-tap filter).
    #[test]
    fn fast_luma_matches_reference() {
        for max in [255i32, 1023] {
            let mut seed = 7u32;
            let d: Vec<u16> = (0..40 * 36)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    ((seed >> 16) as i32 % (max + 1)) as u16
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
    fn chroma_422_blocks_are_bilinear_with_eighth_sample_weights() {
        // 8x16 chroma block with fraction (3, 6) from a linear ramp in both directions is exact
        let d = plane::<u16>(24, 40, |x, y| (x * 8 + y * 16) as i32);
        let p = PlaneRef { data: &d, width: 24, height: 40, stride: 24 };
        let mut out = [0u16; 8 * 16];
        mc_chroma(p, 4, 6, 3, 6, 8, 16, &mut out, 8);
        for r in 0..16 {
            for c in 0..8 {
                // value at (4 + c + 3/8, 6 + r + 6/8): 8 * x + 16 * y, rounded as the filter does
                let want = (8 * (4 + c) * 8 + 8 * 3 + 16 * (6 + r) * 8 + 16 * 6 + 4) / 8;
                assert_eq!(out[r * 8 + c] as usize, want, "{r},{c}");
            }
        }
    }

    #[test]
    fn weighting() {
        let a = [100u8; 4];
        let b = [50u8; 4];
        let mut d = [0u8; 4];
        weighted_store(&mut d, 4, Some(&a), Some(&b), 4, 4, 1, Weight::Default, 255);
        assert_eq!(d, [75; 4]);
        weighted_store(&mut d, 4, Some(&a), None, 4, 4, 1, Weight::Weighted { log_wd: 5, w0: 16, w1: 0, o0: 3, o1: 0 }, 255);
        assert_eq!(d, [53; 4]);
        // wide samples clip at the bit depth
        let a = [1000u16; 4];
        let mut d = [0u16; 4];
        weighted_store(&mut d, 4, Some(&a), None, 4, 4, 1, Weight::Weighted { log_wd: 5, w0: 64, w1: 0, o0: 0, o1: 0 }, 1023);
        assert_eq!(d, [1023; 4]);
    }
}
