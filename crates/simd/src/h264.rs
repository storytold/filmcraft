//! H.264 inter prediction sample interpolation (ITU-T H.264 8.4.2.2) and deblocking edge
//! filters (8.7.2), scalar: the references the vector versions must match (the code
//! filmcraft-h264 used before the vector kernels).

use crate::{H264Edge, H264Out};

const WIN: usize = 16 + 5;

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

/// Horizontal 6-tap (unclipped) at every position of `row` for `n` outputs: out[c] uses row[c..c + 6].
#[inline(always)]
fn htap_row(row: &[u8], out: &mut [i32], n: usize) {
    let row = &row[..n + 5];
    for (o, w) in out[..n].iter_mut().zip(row.windows(6)) {
        *o = tap6(w[0] as i32, w[1] as i32, w[2] as i32, w[3] as i32, w[4] as i32, w[5] as i32);
    }
}

/// Vertical 6-tap (unclipped) over six rows at columns 0..n.
#[inline(always)]
fn vtap_row(rows: [&[u8]; 6], out: &mut [i32], n: usize) {
    let [a, b, c, d, e, f] = rows.map(|r| &r[..n]);
    for i in 0..n {
        out[i] = tap6(a[i] as i32, b[i] as i32, c[i] as i32, d[i] as i32, e[i] as i32, f[i] as i32);
    }
}

/// See [`crate::h264_luma`]: interpolated straight into `dst` (put), or into a block that is
/// then written out.
pub(crate) fn luma_out((win, ws): (&[u8], usize), (fx, fy): (u32, u32), (w, h): (usize, usize), (dst, ds): (&mut [u8], usize), out: H264Out) {
    if out == H264Out::Put {
        return luma((win, ws), (fx, fy), (w, h), (dst, ds));
    }
    let mut pred = [0u8; 16 * 16];
    luma((win, ws), (fx, fy), (w, h), (&mut pred, 16));
    write_out(dst, ds, &pred, 16, w, h, out);
}

/// See [`crate::h264_chroma`].
pub(crate) fn chroma_out((win, ws): (&[u8], usize), (fx, fy): (u32, u32), (w, h): (usize, usize), (dst, ds): (&mut [u8], usize), out: H264Out) {
    if out == H264Out::Put {
        return chroma((win, ws), (fx, fy), (w, h), (dst, ds));
    }
    let mut pred = [0u8; 8 * 8];
    chroma((win, ws), (fx, fy), (w, h), (&mut pred, 8));
    write_out(dst, ds, &pred, 8, w, h, out);
}

/// The w x h prediction `pred` (rows `ps` apart) into `dst` as `out` says (8.4.2.3).
fn write_out(dst: &mut [u8], ds: usize, pred: &[u8], ps: usize, w: usize, h: usize, out: H264Out) {
    for r in 0..h {
        for c in 0..w {
            let (Some(&p), Some(d)) = (pred.get(r * ps + c), dst.get_mut(r * ds + c)) else {
                continue;
            };
            let (p, x) = (p as i32, *d as i32);
            *d = match out {
                H264Out::Put => p as u8,
                H264Out::Avg => ((x + p + 1) >> 1) as u8,
                H264Out::Weight { w, o, log_wd } => {
                    let round = if log_wd >= 1 { 1 << (log_wd - 1) } else { 0 };
                    clip1(((p * w + round) >> log_wd) + o)
                }
                H264Out::WeightBi { w0, w1, o, log_wd } => clip1(((x * w0 + p * w1 + (1 << log_wd)) >> (log_wd + 1)) + o),
            };
        }
    }
}

/// Luma interpolation from a prepared window: `src[0]` is the sample at (x - 2, y - 2) and `ss` the
/// window stride; the window covers (bw + 5) x (bh + 5) samples.
pub(crate) fn luma((src, ss): (&[u8], usize), (fx, fy): (u32, u32), (bw, bh): (usize, usize), (out, os): (&mut [u8], usize)) {
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
                            o[c] = avg(row[c + 2], clip1((t[c] + 16) >> 5));
                        }
                    }
                    2 => {
                        for c in 0..bw {
                            o[c] = clip1((t[c] + 16) >> 5);
                        }
                    }
                    _ => {
                        for c in 0..bw {
                            o[c] = avg(row[c + 3], clip1((t[c] + 16) >> 5));
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
                        o[c] = clip1((t[c] + 16) >> 5);
                    }
                } else {
                    for c in 0..bw {
                        o[c] = avg(g[c], clip1((t[c] + 16) >> 5));
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
                            o[c] = clip1((t[c] + 512) >> 10);
                        }
                    }
                    (2, 1) | (2, 3) => {
                        let bh_ = if fy == 1 { b2 } else { b3 };
                        for c in 0..bw {
                            o[c] = avg(clip1((bh_[c] + 16) >> 5), clip1((t[c] + 512) >> 10));
                        }
                    }
                    _ => {
                        let off = if fx == 1 { 2 } else { 3 };
                        let rows = [wrow(r), wrow(r + 1), wrow(r + 2), wrow(r + 3), wrow(r + 4), wrow(r + 5)].map(|x| &x[off..]);
                        vtap_row(rows, &mut v, bw);
                        for c in 0..bw {
                            o[c] = avg(clip1((v[c] + 16) >> 5), clip1((t[c] + 512) >> 10));
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
                    o[c] = avg(clip1((t[c] + 16) >> 5), clip1((v[c] + 16) >> 5));
                }
            }
        }
    }
}

/// Chroma interpolation from a prepared (bw + 1) x (bh + 1) window whose first sample is the integer
/// position of the block.
pub(crate) fn chroma((src, ss): (&[u8], usize), (fx, fy): (u32, u32), (bw, bh): (usize, usize), (out, os): (&mut [u8], usize)) {
    let (fx, fy) = (fx as u16, fy as u16);
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
            let v = w00 * r0[c] as u16 + w10 * r0[c + 1] as u16 + w01 * r1[c] as u16 + w11 * r1[c + 1] as u16;
            *o = ((v + 32) >> 6) as u8;
        }
    }
}

/// Filter one edge for `N` lines at once (lane `i` = line `i` across the edge). `s[k][i]` holds
/// sample p3 p2 p1 p0 q0 q1 q2 q3 (k = 0..8) of line `i`. Branch-free per lane, so the loops
/// vectorise; bit-exact with [`filter8`] applied to every line (8.7.2.3 / 8.7.2.4).
#[inline(always)]
fn filter_lanes<const N: usize>(s: &mut [[i16; N]; 8], ep: &H264Edge, chroma: bool) {
    let per = N / 4;
    let (alpha, beta) = (ep.alpha as i16, ep.beta as i16);
    let mut bs_on = [false; N];
    let mut tc0 = [0i16; N];
    for k in 0..4 {
        for i in k * per..(k + 1) * per {
            bs_on[i] = ep.bs[k] != 0;
            tc0[i] = ep.tc0[k] as i16;
        }
    }
    let [p3, p2, p1, p0, q0, q1, q2, q3] = *s;
    // Masks are 0 / -1 per lane, selects are bitwise, and clipping to per-lane bounds uses
    // min / max (`clamp` would assert its bounds per lane), so every loop below vectorises.
    let lt = |x: i16, y: i16| -((x < y) as i16);
    let sel = |m: i16, a: i16, b: i16| (a & m) | (b & !m);
    let mut on = [0i16; N];
    let mut any = 0i16;
    for i in 0..N {
        on[i] = -(bs_on[i] as i16) & lt((p0[i] - q0[i]).abs(), alpha) & lt((p1[i] - p0[i]).abs(), beta) & lt((q1[i] - q0[i]).abs(), beta);
        any |= on[i];
    }
    if any == 0 {
        return;
    }
    // bS 4 is only ever assigned to all four segments of an edge
    let strong_edge = ep.bs[0] >= 4;
    if chroma {
        for i in 0..N {
            let (np0, nq0) = if strong_edge {
                ((2 * p1[i] + p0[i] + q1[i] + 2) >> 2, (2 * q1[i] + q0[i] + p1[i] + 2) >> 2)
            } else {
                let tc = tc0[i] + 1;
                let delta = ((((q0[i] - p0[i]) << 2) + (p1[i] - q1[i]) + 4) >> 3).max(-tc).min(tc);
                ((p0[i] + delta).clamp(0, 255), (q0[i] - delta).clamp(0, 255))
            };
            s[3][i] = sel(on[i], np0, p0[i]);
            s[4][i] = sel(on[i], nq0, q0[i]);
        }
        return;
    }
    if strong_edge {
        let lim = (alpha >> 2) + 2;
        for i in 0..N {
            let (a, b, c, d, e, f, g, h) = (p3[i], p2[i], p1[i], p0[i], q0[i], q1[i], q2[i], q3[i]);
            let strong = lt((d - e).abs(), lim);
            let sp = on[i] & lt((b - d).abs(), beta) & strong;
            let sq = on[i] & lt((g - e).abs(), beta) & strong;
            let wp0 = (2 * c + d + f + 2) >> 2;
            let wq0 = (2 * f + e + c + 2) >> 2;
            let sp0 = (b + 2 * c + 2 * d + 2 * e + f + 4) >> 3;
            let sq0 = (c + 2 * d + 2 * e + 2 * f + g + 4) >> 3;
            s[3][i] = sel(sp, sp0, sel(on[i], wp0, d));
            s[2][i] = sel(sp, (b + c + d + e + 2) >> 2, c);
            s[1][i] = sel(sp, (2 * a + 3 * b + c + d + e + 4) >> 3, b);
            s[4][i] = sel(sq, sq0, sel(on[i], wq0, e));
            s[5][i] = sel(sq, (d + e + f + g + 2) >> 2, f);
            s[6][i] = sel(sq, (2 * h + 3 * g + f + e + d + 4) >> 3, g);
        }
    } else {
        for i in 0..N {
            let ap = lt((p2[i] - p0[i]).abs(), beta);
            let aq = lt((q2[i] - q0[i]).abs(), beta);
            let t0 = tc0[i];
            let tc = t0 - ap - aq;
            let delta = ((((q0[i] - p0[i]) << 2) + (p1[i] - q1[i]) + 4) >> 3).max(-tc).min(tc);
            let avg = (p0[i] + q0[i] + 1) >> 1;
            let np1 = p1[i] + ((p2[i] + avg - (p1[i] << 1)) >> 1).max(-t0).min(t0);
            let nq1 = q1[i] + ((q2[i] + avg - (q1[i] << 1)) >> 1).max(-t0).min(t0);
            s[3][i] = sel(on[i], (p0[i] + delta).clamp(0, 255), p0[i]);
            s[4][i] = sel(on[i], (q0[i] - delta).clamp(0, 255), q0[i]);
            s[2][i] = sel(on[i] & ap, np1, p1[i]);
            s[5][i] = sel(on[i] & aq, nq1, q1[i]);
        }
    }
}

/// Filter a vertical edge whose q0 column is at `x`, for `N` lines starting at row `y`.
#[inline(always)]
pub(crate) fn filter_vertical<const N: usize>(pix: &mut [u8], stride: usize, x: usize, y: usize, ep: &H264Edge, chroma: bool) {
    // Each line's eight samples as one little-endian word: byte k = sample k (p3 .. q3).
    let mut w = [0u64; N];
    for (i, w) in w.iter_mut().enumerate() {
        // p3 is 4 left of the edge: nothing to filter at the picture's first sample
        let Some(&b) = ((y + i) * stride + x).checked_sub(4).and_then(|o| pix.get(o..)).and_then(|p| p.first_chunk::<8>()) else {
            return;
        };
        *w = u64::from_le_bytes(b);
    }
    let mut s = [[0i16; N]; 8];
    for (k, row) in s.iter_mut().enumerate() {
        for i in 0..N {
            row[i] = ((w[i] >> (8 * k)) & 0xff) as i16;
        }
    }
    filter_lanes(&mut s, ep, chroma);
    for (i, w) in w.iter_mut().enumerate() {
        *w = (0..8).fold(0u64, |acc, k| acc | ((s[k][i] as u8 as u64) << (8 * k)));
    }
    for (i, w) in w.iter().enumerate() {
        let o = (y + i) * stride + x - 4;
        pix[o..o + 8].copy_from_slice(&w.to_le_bytes());
    }
}

/// Filter a horizontal edge whose q0 row is `y`, for `N` columns starting at `x`.
#[inline(always)]
pub(crate) fn filter_horizontal<const N: usize>(pix: &mut [u8], stride: usize, x: usize, y: usize, ep: &H264Edge, chroma: bool) {
    // p3 is 4 rows above the edge: nothing to filter on the picture's first rows
    let Some(base) = y.checked_sub(4).map(|r| r * stride + x) else {
        return;
    };
    let mut s = [[0i16; N]; 8];
    for (k, row) in s.iter_mut().enumerate() {
        let Some(src) = pix.get(base + k * stride..).and_then(|p| p.first_chunk::<N>()) else {
            return;
        };
        for i in 0..N {
            row[i] = src[i] as i16;
        }
    }
    filter_lanes(&mut s, ep, chroma);
    let (k0, k1) = if chroma { (3, 5) } else { (1, 7) };
    for (k, row) in s.iter().enumerate().take(k1).skip(k0) {
        let Some(dst) = pix.get_mut(base + k * stride..).and_then(|p| p.first_chunk_mut::<N>()) else {
            return;
        };
        for i in 0..N {
            dst[i] = row[i] as u8;
        }
    }
}
