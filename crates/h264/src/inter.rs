//! Inter prediction sample interpolation (8.4.2.2) and weighted sample prediction (8.4.2.3).

/// A reference plane (used by the plane-based helpers in tests).
#[cfg(test)]
#[derive(Clone, Copy)]
pub struct PlaneRef<'a> {
    pub data: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

const WIN: usize = 16 + 5;

/// Copy the (bw+5)x(bh+5) window whose top-left is (x-2, y-2) into `win` (stride WIN), clamping
/// coordinates to the picture.
#[cfg(test)]
#[inline]
fn fetch_window(p: PlaneRef, x: i32, y: i32, bw: usize, bh: usize, win: &mut [u8; WIN * WIN]) {
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
fn clip1(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Copy `n` bytes with a fixed-size move for the common block widths.
#[inline(always)]
fn copy_n(dst: &mut [u8], src: &[u8], n: usize) {
    fn fixed<const N: usize>(dst: &mut [u8], src: &[u8]) {
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
fn avg(a: u8, b: u8) -> u8 {
    ((a as u16 + b as u16 + 1) >> 1) as u8
}

/// 6-tap filter in i16: the unclipped result of a first pass lies in -2550..=10710.
#[inline(always)]
fn tap6_16(a: u8, b: u8, c: u8, d: u8, e: u8, f: u8) -> i16 {
    let [a, b, c, d, e, f] = [a, b, c, d, e, f].map(i16::from);
    a - 5 * b + 20 * c + 20 * d - 5 * e + f
}

/// Round and clip a first-pass value: Clip1((v + 16) >> 5).
#[inline(always)]
fn round5(v: i16) -> u8 {
    ((v + 16) >> 5).clamp(0, 255) as u8
}

/// Horizontal 6-tap (unclipped) for `BW` outputs: out[c] uses row[c..c + 6].
#[inline(always)]
fn htap_row<const BW: usize>(row: &[u8], out: &mut [i16; 16]) {
    let row = &row[..BW + 5];
    for c in 0..BW {
        out[c] = tap6_16(row[c], row[c + 1], row[c + 2], row[c + 3], row[c + 4], row[c + 5]);
    }
}

/// Vertical 6-tap (unclipped) over six rows at columns 0..BW.
#[inline(always)]
fn vtap_row<const BW: usize>(rows: [&[u8]; 6], out: &mut [i16; 16]) {
    let [a, b, c, d, e, f] = rows.map(|r| &r[..BW]);
    for i in 0..BW {
        out[i] = tap6_16(a[i], b[i], c[i], d[i], e[i], f[i]);
    }
}

#[inline(always)]
fn copy_fixed<const BW: usize>(dst: &mut [u8], src: &[u8]) {
    if let (Some(d), Some(s)) = (dst.first_chunk_mut::<BW>(), src.first_chunk::<BW>()) {
        *d = *s;
    }
}

/// Luma sample interpolation for a bw x bh block (8.4.2.2.1). (x, y) is the integer sample position
/// (block position + (mv >> 2)); (fx, fy) the quarter-sample fraction. Output stride is `os`.
#[cfg(test)]
pub fn mc_luma(p: PlaneRef, x: i32, y: i32, blk: McBlock, out: &mut [u8], os: usize) {
    let McBlock { bw, bh, .. } = blk;
    let mut win = [0u8; WIN * WIN];
    let (x0, y0) = (x - 2, y - 2);
    let inside = x0 >= 0 && y0 >= 0 && x0 as usize + bw + 5 <= p.width && y0 as usize + bh + 5 <= p.height;
    let (src, ss): (&[u8], usize) = if inside {
        (&p.data[y0 as usize * p.stride + x0 as usize..], p.stride)
    } else {
        fetch_window(p, x, y, bw, bh, &mut win);
        (&win[..], WIN)
    };
    mc_luma_win(src, ss, blk, out, os);
}

/// Luma interpolation from a prepared window: `src[0]` is the sample at (x - 2, y - 2) and `ss` the
/// window stride; the window covers (bw + 5) x (bh + 5) samples. `bw` is 4, 8 or 16.
pub fn mc_luma_win(src: &[u8], ss: usize, blk: McBlock, out: &mut [u8], os: usize) {
    let McBlock { fx, fy, bw, bh } = blk;
    match bw {
        16 => luma_win::<16>(src, ss, fx, fy, bh, out, os),
        8 => luma_win::<8>(src, ss, fx, fy, bh, out, os),
        _ => luma_win::<4>(src, ss, fx, fy, bh, out, os),
    }
}

#[inline(always)]
fn luma_win<const BW: usize>(src: &[u8], ss: usize, fx: u32, fy: u32, bh: usize, out: &mut [u8], os: usize) {
    // window row r (0..bh+5) starts at src[r * ss]; sample G of block (r, c) is at window (r + 2, c + 2)
    let wrow = |r: usize| &src[r * ss..r * ss + BW + 5];
    let rows6 = |r: usize, off: usize| [wrow(r), wrow(r + 1), wrow(r + 2), wrow(r + 3), wrow(r + 4), wrow(r + 5)].map(|x| &x[off..]);
    let mut t = [0i16; 16];
    match (fx, fy) {
        (0, 0) => {
            for r in 0..bh {
                copy_fixed::<BW>(&mut out[r * os..], &wrow(r + 2)[2..]);
            }
        }
        (_, 0) => {
            let g = if fx == 1 { 2 } else { 3 };
            for r in 0..bh {
                let row = wrow(r + 2);
                htap_row::<BW>(row, &mut t);
                let o = &mut out[r * os..r * os + BW];
                if fx == 2 {
                    for c in 0..BW {
                        o[c] = round5(t[c]);
                    }
                } else {
                    let g = &row[g..g + BW];
                    for c in 0..BW {
                        o[c] = avg(g[c], round5(t[c]));
                    }
                }
            }
        }
        (0, _) => {
            for r in 0..bh {
                let rows = rows6(r, 2);
                vtap_row::<BW>(rows, &mut t);
                let o = &mut out[r * os..r * os + BW];
                if fy == 2 {
                    for c in 0..BW {
                        o[c] = round5(t[c]);
                    }
                } else {
                    let g = &(if fy == 1 { rows[2] } else { rows[3] })[..BW];
                    for c in 0..BW {
                        o[c] = avg(g[c], round5(t[c]));
                    }
                }
            }
        }
        (2, _) | (_, 2) => {
            // unclipped horizontal half samples b1 for window rows 0..bh+5
            let mut b1 = [[0i16; 16]; WIN];
            for (r, bt) in b1.iter_mut().enumerate().take(bh + 5) {
                htap_row::<BW>(wrow(r), bt);
            }
            let mut j = [0u8; 16];
            let mut v = [0i16; 16];
            for r in 0..bh {
                let o = &mut out[r * os..r * os + BW];
                let [b0, b1r, b2, b3, b4, b5] = [&b1[r], &b1[r + 1], &b1[r + 2], &b1[r + 3], &b1[r + 4], &b1[r + 5]];
                for c in 0..BW {
                    let [a, b, cc, d, e, f] = [b0[c], b1r[c], b2[c], b3[c], b4[c], b5[c]].map(i32::from);
                    j[c] = clip1((tap6(a, b, cc, d, e, f) + 512) >> 10);
                }
                match (fx, fy) {
                    (2, 2) => o.copy_from_slice(&j[..BW]),
                    (2, 1) | (2, 3) => {
                        let bh_ = if fy == 1 { b2 } else { b3 };
                        for c in 0..BW {
                            o[c] = avg(round5(bh_[c]), j[c]);
                        }
                    }
                    _ => {
                        vtap_row::<BW>(rows6(r, if fx == 1 { 2 } else { 3 }), &mut v);
                        for c in 0..BW {
                            o[c] = avg(round5(v[c]), j[c]);
                        }
                    }
                }
            }
        }
        _ => {
            // e, g, p, r: average of a horizontal half sample (row r or r + 1) and a vertical one (col c or c + 1)
            let hoff = if fy == 1 { 2 } else { 3 };
            let voff = if fx == 1 { 2 } else { 3 };
            let mut v = [0i16; 16];
            for r in 0..bh {
                htap_row::<BW>(wrow(r + hoff), &mut t);
                vtap_row::<BW>(rows6(r, voff), &mut v);
                let o = &mut out[r * os..r * os + BW];
                for c in 0..BW {
                    o[c] = avg(round5(t[c]), round5(v[c]));
                }
            }
        }
    }
}

/// Chroma sample interpolation (8.4.2.2.2) for a bw x bh block at integer position (x, y) with
/// eighth-sample fraction (fx, fy).
#[cfg(test)]
pub fn mc_chroma(p: PlaneRef, x: i32, y: i32, blk: McBlock, out: &mut [u8], os: usize) {
    let McBlock { bw, bh, .. } = blk;
    let maxx = p.width as i32 - 1;
    let maxy = p.height as i32 - 1;
    let inside = x >= 0 && y >= 0 && x + bw as i32 <= maxx && y + bh as i32 <= maxy;
    // (bw+1) x (bh+1) source window; copied with edge clamping when it crosses the picture border
    let mut win = [0u8; 9 * 9];
    let (src, ss): (&[u8], usize) = if inside {
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
    mc_chroma_win(src, ss, blk, out, os);
}

/// Chroma interpolation from a prepared (bw + 1) x (bh + 1) window whose first sample is the integer
/// position of the block. `bw` is 2, 4 or 8.
pub fn mc_chroma_win(src: &[u8], ss: usize, blk: McBlock, out: &mut [u8], os: usize) {
    let McBlock { fx, fy, bw, bh } = blk;
    match bw {
        8 => chroma_win::<8>(src, ss, fx, fy, bh, out, os),
        4 => chroma_win::<4>(src, ss, fx, fy, bh, out, os),
        _ => chroma_win::<2>(src, ss, fx, fy, bh, out, os),
    }
}

#[inline(always)]
fn chroma_win<const BW: usize>(src: &[u8], ss: usize, fx: u32, fy: u32, bh: usize, out: &mut [u8], os: usize) {
    if fx == 0 && fy == 0 {
        for r in 0..bh {
            copy_fixed::<BW>(&mut out[r * os..], &src[r * ss..]);
        }
        return;
    }
    let (fx, fy) = (fx as u16, fy as u16);
    let (w00, w10, w01, w11) = ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
    for r in 0..bh {
        let r0 = &src[r * ss..r * ss + BW + 1];
        let r1 = &src[(r + 1) * ss..(r + 1) * ss + BW + 1];
        let o = &mut out[r * os..r * os + BW];
        for c in 0..BW {
            let v = w00 * r0[c] as u16 + w10 * r0[c + 1] as u16 + w01 * r1[c] as u16 + w11 * r1[c + 1] as u16;
            o[c] = ((v + 32) >> 6) as u8;
        }
    }
}

/// Fractional offset and size of one motion-compensated block.
#[derive(Clone, Copy, Debug)]
pub struct McBlock {
    pub fx: u32,
    pub fy: u32,
    pub bw: usize,
    pub bh: usize,
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
pub fn weighted_store(dst: &mut [u8], ds: usize, p0: Option<&[u8]>, p1: Option<&[u8]>, ps: usize, (bw, bh): (usize, usize), w: Weight) {
    // equal weights 1 << logWD with no offset (implicit 32/32) reduce exactly to the rounded average
    let w = match w {
        Weight::Weighted { log_wd, w0, w1, o0, o1 } if p0.is_some() && p1.is_some() && w0 == w1 && w0 == 1 << log_wd && (o0 + o1 + 1) >> 1 == 0 => {
            Weight::Default
        }
        w => w,
    };
    match (p0, p1, w) {
        (Some(a), Some(b), Weight::Default) => {
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                let rb = &b[r * ps..r * ps + bw];
                for ((d, &x), &y) in d.iter_mut().zip(ra).zip(rb) {
                    *d = ((x as u32 + y as u32 + 1) >> 1) as u8;
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
                    *d = clip1(((x as i32 * w0 + y as i32 * w1 + round) >> sh) + off);
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
                    *d = clip1(((x as i32 * wt + round) >> log_wd) + o);
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
    pub fn mc_luma_ref(p: PlaneRef, x: i32, y: i32, blk: McBlock, out: &mut [u8], os: usize) {
        let McBlock { fx, fy, bw, bh } = blk;
        let mut win = [0u8; WIN * WIN];
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
                        let b = clip1((b1(r as isize, c) + 16) >> 5) as i32;
                        out[r * os + c] = match fx {
                            1 => ((g(r, c) + b + 1) >> 1) as u8,
                            2 => b as u8,
                            _ => ((g(r, c + 1) + b + 1) >> 1) as u8,
                        };
                    }
                }
            }
            (0, _) => {
                // d, h, n
                for r in 0..bh {
                    for c in 0..bw {
                        let h = clip1((h1(r, c as isize) + 16) >> 5) as i32;
                        out[r * os + c] = match fy {
                            1 => ((g(r, c) + h + 1) >> 1) as u8,
                            2 => h as u8,
                            _ => ((g(r + 1, c) + h + 1) >> 1) as u8,
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
                        let j = clip1((j1 + 512) >> 10) as i32;
                        let v = match (fx, fy) {
                            (2, 2) => j,
                            (2, 1) => (clip1((bcol[(r + 2) * WIN + c] + 16) >> 5) as i32 + j + 1) >> 1, // f
                            (2, 3) => (clip1((bcol[(r + 3) * WIN + c] + 16) >> 5) as i32 + j + 1) >> 1, // q
                            (1, 2) => (clip1((h1(r, c as isize) + 16) >> 5) as i32 + j + 1) >> 1,       // i
                            _ => (clip1((h1(r, c as isize + 1) + 16) >> 5) as i32 + j + 1) >> 1,        // k
                        };
                        out[r * os + c] = v as u8;
                    }
                }
            }
            _ => {
                // e, g, p, r: average of a horizontal half-sample (b or s) and a vertical one (h or m)
                for r in 0..bh {
                    for c in 0..bw {
                        let hr = if fy == 1 { r as isize } else { r as isize + 1 };
                        let vc = if fx == 1 { c as isize } else { c as isize + 1 };
                        let bh_ = clip1((b1(hr, c) + 16) >> 5) as i32;
                        let vv = clip1((h1(r, vc) + 16) >> 5) as i32;
                        out[r * os + c] = ((bh_ + vv + 1) >> 1) as u8;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        (0..w * h).map(|i| f(i % w, i / w)).collect()
    }

    #[test]
    fn full_sample_copy_and_clamp() {
        let d = plane(16, 16, |x, y| (x + 16 * y) as u8);
        let p = PlaneRef { data: &d, width: 16, height: 16, stride: 16 };
        let mut out = [0u8; 16];
        mc_luma(p, 2, 3, McBlock { fx: 0, fy: 0, bw: 4, bh: 4 }, &mut out, 4);
        assert_eq!(&out[..4], &[50, 51, 52, 53]);
        // clamped: far outside to the top-left gives sample (0,0)
        mc_luma(p, -40, -40, McBlock { fx: 0, fy: 0, bw: 4, bh: 4 }, &mut out, 4);
        assert!(out.iter().all(|&v| v == 0));
    }

    #[test]
    fn half_sample_constant_is_constant() {
        let d = vec![100u8; 32 * 32];
        let p = PlaneRef { data: &d, width: 32, height: 32, stride: 32 };
        for fx in 0..4 {
            for fy in 0..4 {
                let mut out = [0u8; 64];
                mc_luma(p, 8, 8, McBlock { fx, fy, bw: 8, bh: 8 }, &mut out, 8);
                assert!(out.iter().all(|&v| v == 100), "{fx},{fy}");
                mc_chroma(p, 8, 8, McBlock { fx: fx * 2, fy: fy * 2, bw: 8, bh: 8 }, &mut out, 8);
                assert!(out.iter().all(|&v| v == 100));
            }
        }
    }

    #[test]
    fn half_sample_horizontal_ramp() {
        // linear ramp: 6-tap filter of a linear function is exact: b = (G + H) / 2 (rounded)
        let d = plane(32, 32, |x, _| (x * 4) as u8);
        let p = PlaneRef { data: &d, width: 32, height: 32, stride: 32 };
        let mut out = [0u8; 16];
        mc_luma(p, 8, 8, McBlock { fx: 2, fy: 0, bw: 4, bh: 4 }, &mut out, 4);
        assert_eq!(&out[..4], &[34, 38, 42, 46]);
        mc_luma(p, 8, 8, McBlock { fx: 1, fy: 0, bw: 4, bh: 4 }, &mut out, 4);
        assert_eq!(&out[..4], &[33, 37, 41, 45]);
    }

    #[test]
    fn fast_luma_matches_reference() {
        let mut seed = 7u32;
        let d: Vec<u8> = (0..40 * 36)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8
            })
            .collect();
        let p = PlaneRef { data: &d, width: 40, height: 36, stride: 40 };
        for &(bw, bh) in &[(16, 16), (16, 8), (8, 16), (8, 8), (8, 4), (4, 8), (4, 4)] {
            for fx in 0..4 {
                for fy in 0..4 {
                    for &(x, y) in &[(10, 9), (-3, -5), (30, 30), (0, 0), (2, 2), (38, 1)] {
                        let mut a = [0u8; 256];
                        let mut b = [0u8; 256];
                        mc_luma(p, x, y, McBlock { fx, fy, bw, bh }, &mut a, 16);
                        reference::mc_luma_ref(p, x, y, McBlock { fx, fy, bw, bh }, &mut b, 16);
                        assert_eq!(a, b, "{bw}x{bh} frac {fx},{fy} at {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn chroma_matches_formula() {
        let mut seed = 11u32;
        let src: Vec<u8> = (0..9 * 9)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8
            })
            .collect();
        for bw in [2, 4, 8] {
            for bh in [2, 4, 8] {
                for fx in 0..8 {
                    for fy in 0..8 {
                        let mut out = [0u8; 64];
                        mc_chroma_win(&src, 9, McBlock { fx, fy, bw, bh }, &mut out, 8);
                        for r in 0..bh {
                            for c in 0..bw {
                                let s = |dr: usize, dc: usize| src[(r + dr) * 9 + c + dc] as u32;
                                let want = ((8 - fx) * (8 - fy) * s(0, 0) + fx * (8 - fy) * s(0, 1) + (8 - fx) * fy * s(1, 0) + fx * fy * s(1, 1) + 32) >> 6;
                                assert_eq!(out[r * 8 + c] as u32, want, "{bw}x{bh} frac {fx},{fy} at {r},{c}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn weighting() {
        let a = [100u8; 4];
        let b = [50u8; 4];
        let mut d = [0u8; 4];
        weighted_store(&mut d, 4, Some(&a), Some(&b), 4, (4, 1), Weight::Default);
        assert_eq!(d, [75; 4]);
        weighted_store(&mut d, 4, Some(&a), None, 4, (4, 1), Weight::Weighted { log_wd: 5, w0: 16, w1: 0, o0: 3, o1: 0 });
        assert_eq!(d, [53; 4]);
    }

    #[test]
    fn equal_power_of_two_weights_are_the_average() {
        let a: Vec<u8> = (0..=255).collect();
        let b: Vec<u8> = (0..=255).rev().map(|v: u8| v.wrapping_mul(7)).collect();
        for (log_wd, o0, o1) in [(5, 0, 0), (0, 0, 0), (7, 0, 0), (3, 1, -2), (2, -1, 0)] {
            let w0 = 1 << log_wd;
            let mut fast = [0u8; 256];
            weighted_store(&mut fast, 16, Some(&a), Some(&b), 16, (16, 16), Weight::Weighted { log_wd, w0, w1: w0, o0, o1 });
            let mut want = [0u8; 256];
            for i in 0..256 {
                want[i] = clip1(((a[i] as i32 * w0 + b[i] as i32 * w0 + (1 << log_wd)) >> (log_wd + 1)) + ((o0 + o1 + 1) >> 1));
            }
            assert_eq!(fast, want, "logWD {log_wd}");
        }
    }
}
