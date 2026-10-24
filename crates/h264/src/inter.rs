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

/// ITU-T H.264 6-tap luma FIR on eight lanes: `a − 5b + 20c + 20d − 5e + f`.
#[inline(always)]
fn tap6_x8(a: wide::i32x8, b: wide::i32x8, c: wide::i32x8, d: wide::i32x8, e: wide::i32x8, f: wide::i32x8) -> wide::i32x8 {
    let five = wide::i32x8::splat(5);
    let twenty = wide::i32x8::splat(20);
    a - b * five + c * twenty + d * twenty - e * five + f
}

/// Load eight consecutive 8-bit samples as `i32x8` (no `unsafe`; `src` must hold ≥ 8 bytes).
#[inline(always)]
fn load8_u8_i32(src: &[u8]) -> wide::i32x8 {
    wide::i32x8::from([
        src[0] as i32,
        src[1] as i32,
        src[2] as i32,
        src[3] as i32,
        src[4] as i32,
        src[5] as i32,
        src[6] as i32,
        src[7] as i32,
    ])
}

#[inline(always)]
fn store8_i32(dst: &mut [i32], v: wide::i32x8) {
    let a: [i32; 8] = v.into();
    dst[..8].copy_from_slice(&a);
}

#[inline(always)]
fn clip1(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Clip eight `i32` lanes to 8-bit and store.
#[inline(always)]
fn store8_clip_u8(dst: &mut [u8], v: wide::i32x8) {
    let a: [i32; 8] = v.max(wide::i32x8::splat(0)).min(wide::i32x8::splat(255)).into();
    for i in 0..8 {
        dst[i] = a[i] as u8;
    }
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
fn htap_row_scalar(row: &[u8], out: &mut [i32], n: usize) {
    let row = &row[..n + 5];
    for (o, w) in out[..n].iter_mut().zip(row.windows(6)) {
        *o = tap6(w[0] as i32, w[1] as i32, w[2] as i32, w[3] as i32, w[4] as i32, w[5] as i32);
    }
}

/// Horizontal 6-tap: eight-wide `wide::i32x8` body, scalar tail for remainders / widths < 8.
#[inline(always)]
fn htap_row(row: &[u8], out: &mut [i32], n: usize) {
    let row = &row[..n + 5];
    let mut i = 0;
    while i + 8 <= n {
        let a = load8_u8_i32(&row[i..]);
        let b = load8_u8_i32(&row[i + 1..]);
        let c = load8_u8_i32(&row[i + 2..]);
        let d = load8_u8_i32(&row[i + 3..]);
        let e = load8_u8_i32(&row[i + 4..]);
        let f = load8_u8_i32(&row[i + 5..]);
        store8_i32(&mut out[i..], tap6_x8(a, b, c, d, e, f));
        i += 8;
    }
    if i < n {
        htap_row_scalar(&row[i..], &mut out[i..], n - i);
    }
}

/// Vertical 6-tap (unclipped) over six rows at columns 0..n.
#[inline(always)]
fn vtap_row_scalar(rows: [&[u8]; 6], out: &mut [i32], n: usize) {
    let [a, b, c, d, e, f] = rows.map(|r| &r[..n]);
    for i in 0..n {
        out[i] = tap6(a[i] as i32, b[i] as i32, c[i] as i32, d[i] as i32, e[i] as i32, f[i] as i32);
    }
}

/// Vertical 6-tap: eight-wide `wide::i32x8` body, scalar tail for remainders / widths < 8.
#[inline(always)]
fn vtap_row(rows: [&[u8]; 6], out: &mut [i32], n: usize) {
    let [a, b, c, d, e, f] = rows.map(|r| &r[..n]);
    let mut i = 0;
    while i + 8 <= n {
        store8_i32(
            &mut out[i..],
            tap6_x8(
                load8_u8_i32(&a[i..]),
                load8_u8_i32(&b[i..]),
                load8_u8_i32(&c[i..]),
                load8_u8_i32(&d[i..]),
                load8_u8_i32(&e[i..]),
                load8_u8_i32(&f[i..]),
            ),
        );
        i += 8;
    }
    if i < n {
        vtap_row_scalar([a, b, c, d, e, f].map(|r| &r[i..]), &mut out[i..], n - i);
    }
}

/// Luma sample interpolation for a bw x bh block (8.4.2.2.1). (x, y) is the integer sample position
/// (block position + (mv >> 2)); (fx, fy) the quarter-sample fraction. Output stride is `os`.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn mc_luma(p: PlaneRef, x: i32, y: i32, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u8], os: usize) {
    let mut win = [0u8; WIN * WIN];
    let (x0, y0) = (x - 2, y - 2);
    let inside = x0 >= 0 && y0 >= 0 && x0 as usize + bw + 5 <= p.width && y0 as usize + bh + 5 <= p.height;
    let (src, ss): (&[u8], usize) = if inside {
        (&p.data[y0 as usize * p.stride + x0 as usize..], p.stride)
    } else {
        fetch_window(p, x, y, bw, bh, &mut win);
        (&win[..], WIN)
    };
    mc_luma_win(src, ss, fx, fy, bw, bh, out, os);
}

/// Luma interpolation from a prepared window: `src[0]` is the sample at (x - 2, y - 2) and `ss` the
/// window stride; the window covers (bw + 5) x (bh + 5) samples.
#[allow(clippy::too_many_arguments)]
pub fn mc_luma_win(src: &[u8], ss: usize, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u8], os: usize) {
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

/// Chroma sample interpolation (8.4.2.2.2) for a bw x bh block at integer position (x, y) with
/// eighth-sample fraction (fx, fy).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma(p: PlaneRef, x: i32, y: i32, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u8], os: usize) {
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
    mc_chroma_win(src, ss, fx, fy, bw, bh, out, os);
}

/// Chroma interpolation from a prepared (bw + 1) x (bh + 1) window whose first sample is the integer
/// position of the block.
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma_win(src: &[u8], ss: usize, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u8], os: usize) {
    let (fx, fy) = (fx as u16, fy as u16);
    let (w00, w10, w01, w11) = ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
    if fx == 0 && fy == 0 {
        for r in 0..bh {
            copy_n(&mut out[r * os..], &src[r * ss..], bw);
        }
        return;
    }
    let w00s = wide::i32x8::splat(w00 as i32);
    let w10s = wide::i32x8::splat(w10 as i32);
    let w01s = wide::i32x8::splat(w01 as i32);
    let w11s = wide::i32x8::splat(w11 as i32);
    let thirty_two = wide::i32x8::splat(32);
    for r in 0..bh {
        let r0 = &src[r * ss..r * ss + bw + 1];
        let r1 = &src[(r + 1) * ss..(r + 1) * ss + bw + 1];
        let o = &mut out[r * os..r * os + bw];
        let mut c = 0;
        while c + 8 <= bw {
            let v = w00s * load8_u8_i32(&r0[c..])
                + w10s * load8_u8_i32(&r0[c + 1..])
                + w01s * load8_u8_i32(&r1[c..])
                + w11s * load8_u8_i32(&r1[c + 1..]);
            let a: [i32; 8] = ((v + thirty_two) >> 6u32).into();
            for i in 0..8 {
                o[c + i] = a[i] as u8;
            }
            c += 8;
        }
        while c < bw {
            let v = w00 * r0[c] as u16 + w10 * r0[c + 1] as u16 + w01 * r1[c] as u16 + w11 * r1[c + 1] as u16;
            o[c] = ((v + 32) >> 6) as u8;
            c += 1;
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
pub fn weighted_store(dst: &mut [u8], ds: usize, p0: Option<&[u8]>, p1: Option<&[u8]>, ps: usize, bw: usize, bh: usize, w: Weight) {
    match (p0, p1, w) {
        (Some(a), Some(b), Weight::Default) => {
            let one = wide::i32x8::splat(1);
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                let rb = &b[r * ps..r * ps + bw];
                let mut c = 0;
                while c + 8 <= bw {
                    let avg: wide::i32x8 = (load8_u8_i32(&ra[c..]) + load8_u8_i32(&rb[c..]) + one) >> 1u32;
                    let t: [i32; 8] = avg.into();
                    for i in 0..8 {
                        d[c + i] = t[i] as u8;
                    }
                    c += 8;
                }
                while c < bw {
                    d[c] = ((ra[c] as u32 + rb[c] as u32 + 1) >> 1) as u8;
                    c += 1;
                }
            }
        }
        (Some(a), Some(b), Weight::Weighted { log_wd, w0, w1, o0, o1 }) => {
            let round = 1 << log_wd;
            let off = (o0 + o1 + 1) >> 1;
            let sh = log_wd + 1;
            let w0s = wide::i32x8::splat(w0);
            let w1s = wide::i32x8::splat(w1);
            let rounds = wide::i32x8::splat(round);
            let offs = wide::i32x8::splat(off);
            let shu = sh as u32;
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                let rb = &b[r * ps..r * ps + bw];
                let mut c = 0;
                while c + 8 <= bw {
                    let v: wide::i32x8 = (load8_u8_i32(&ra[c..]) * w0s + load8_u8_i32(&rb[c..]) * w1s + rounds) >> shu;
                    store8_clip_u8(&mut d[c..], v + offs);
                    c += 8;
                }
                while c < bw {
                    d[c] = clip1(((ra[c] as i32 * w0 + rb[c] as i32 * w1 + round) >> sh) + off);
                    c += 1;
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
            let wts = wide::i32x8::splat(wt);
            let rounds = wide::i32x8::splat(round);
            let os = wide::i32x8::splat(o);
            let shu = log_wd as u32;
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                let mut c = 0;
                while c + 8 <= bw {
                    store8_clip_u8(&mut d[c..], ((load8_u8_i32(&ra[c..]) * wts + rounds) >> shu) + os);
                    c += 8;
                }
                while c < bw {
                    d[c] = clip1(((ra[c] as i32 * wt + round) >> log_wd) + o);
                    c += 1;
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
    pub fn mc_luma_ref(p: PlaneRef, x: i32, y: i32, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u8], os: usize) {
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
        mc_luma(p, 2, 3, 0, 0, 4, 4, &mut out, 4);
        assert_eq!(&out[..4], &[50, 51, 52, 53]);
        // clamped: far outside to the top-left gives sample (0,0)
        mc_luma(p, -40, -40, 0, 0, 4, 4, &mut out, 4);
        assert!(out.iter().all(|&v| v == 0));
    }

    #[test]
    fn half_sample_constant_is_constant() {
        let d = vec![100u8; 32 * 32];
        let p = PlaneRef { data: &d, width: 32, height: 32, stride: 32 };
        for fx in 0..4 {
            for fy in 0..4 {
                let mut out = [0u8; 64];
                mc_luma(p, 8, 8, fx, fy, 8, 8, &mut out, 8);
                assert!(out.iter().all(|&v| v == 100), "{fx},{fy}");
                mc_chroma(p, 8, 8, fx * 2, fy * 2, 8, 8, &mut out, 8);
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
        mc_luma(p, 8, 8, 2, 0, 4, 4, &mut out, 4);
        assert_eq!(&out[..4], &[34, 38, 42, 46]);
        mc_luma(p, 8, 8, 1, 0, 4, 4, &mut out, 4);
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
                        mc_luma(p, x, y, fx, fy, bw, bh, &mut a, 16);
                        reference::mc_luma_ref(p, x, y, fx, fy, bw, bh, &mut b, 16);
                        assert_eq!(a, b, "{bw}x{bh} frac {fx},{fy} at {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn six_tap_simd_matches_scalar() {
        let mut seed = 0x9e37_79b9u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as u8
        };
        for n in [4usize, 8, 16, 5, 9, 15, 17] {
            let mut row = vec![0u8; n + 5];
            for x in &mut row {
                *x = next();
            }
            let mut simd = vec![0i32; n];
            let mut scalar = vec![0i32; n];
            htap_row(&row, &mut simd, n);
            htap_row_scalar(&row, &mut scalar, n);
            assert_eq!(simd, scalar, "htap_row n={n}");

            let rows: [Vec<u8>; 6] = std::array::from_fn(|_| (0..n).map(|_| next()).collect());
            let refs: [&[u8]; 6] = std::array::from_fn(|i| rows[i].as_slice());
            let mut vs = vec![0i32; n];
            let mut vc = vec![0i32; n];
            vtap_row(refs, &mut vs, n);
            vtap_row_scalar(refs, &mut vc, n);
            assert_eq!(vs, vc, "vtap_row n={n}");
        }
    }

    #[test]
    fn weighting() {
        let a = [100u8; 4];
        let b = [50u8; 4];
        let mut d = [0u8; 4];
        weighted_store(&mut d, 4, Some(&a), Some(&b), 4, 4, 1, Weight::Default);
        assert_eq!(d, [75; 4]);
        weighted_store(&mut d, 4, Some(&a), None, 4, 4, 1, Weight::Weighted { log_wd: 5, w0: 16, w1: 0, o0: 3, o1: 0 });
        assert_eq!(d, [53; 4]);
    }

    fn chroma_scalar(src: &[u8], ss: usize, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u8], os: usize) {
        let (fx, fy) = (fx as u16, fy as u16);
        let (w00, w10, w01, w11) = ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
        for r in 0..bh {
            let r0 = &src[r * ss..r * ss + bw + 1];
            let r1 = &src[(r + 1) * ss..(r + 1) * ss + bw + 1];
            let o = &mut out[r * os..r * os + bw];
            for c in 0..bw {
                let v = w00 * r0[c] as u16 + w10 * r0[c + 1] as u16 + w01 * r1[c] as u16 + w11 * r1[c + 1] as u16;
                o[c] = ((v + 32) >> 6) as u8;
            }
        }
    }

    fn weighted_store_scalar(dst: &mut [u8], ds: usize, p0: Option<&[u8]>, p1: Option<&[u8]>, ps: usize, bw: usize, bh: usize, w: Weight) {
        match (p0, p1, w) {
            (Some(a), Some(b), Weight::Default) => {
                for r in 0..bh {
                    for c in 0..bw {
                        dst[r * ds + c] = ((a[r * ps + c] as u32 + b[r * ps + c] as u32 + 1) >> 1) as u8;
                    }
                }
            }
            (Some(a), Some(b), Weight::Weighted { log_wd, w0, w1, o0, o1 }) => {
                let round = 1 << log_wd;
                let off = (o0 + o1 + 1) >> 1;
                let sh = log_wd + 1;
                for r in 0..bh {
                    for c in 0..bw {
                        dst[r * ds + c] = clip1(((a[r * ps + c] as i32 * w0 + b[r * ps + c] as i32 * w1 + round) >> sh) + off);
                    }
                }
            }
            (Some(a), None, Weight::Default) | (None, Some(a), Weight::Default) => {
                for r in 0..bh {
                    dst[r * ds..r * ds + bw].copy_from_slice(&a[r * ps..r * ps + bw]);
                }
            }
            (Some(a), None, Weight::Weighted { w0, o0, log_wd, .. }) => {
                let round = if log_wd >= 1 { 1 << (log_wd - 1) } else { 0 };
                for r in 0..bh {
                    for c in 0..bw {
                        dst[r * ds + c] = clip1(((a[r * ps + c] as i32 * w0 + round) >> log_wd) + o0);
                    }
                }
            }
            (None, Some(a), Weight::Weighted { w1, o1, log_wd, .. }) => {
                let round = if log_wd >= 1 { 1 << (log_wd - 1) } else { 0 };
                for r in 0..bh {
                    for c in 0..bw {
                        dst[r * ds + c] = clip1(((a[r * ps + c] as i32 * w1 + round) >> log_wd) + o1);
                    }
                }
            }
            (None, None, _) => {}
        }
    }

    #[test]
    fn chroma_simd_matches_scalar() {
        let mut seed = 0x51aa_e995u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as u8
        };
        for &(bw, bh) in &[(4, 4), (8, 8), (8, 4), (16, 8), (16, 16), (2, 2), (9, 3)] {
            let ss = bw + 1;
            let src: Vec<u8> = (0..(bh + 1) * ss).map(|_| next()).collect();
            for fx in 0..8u32 {
                for fy in 0..8u32 {
                    let mut a = vec![0u8; bh * 16];
                    let mut b = vec![0u8; bh * 16];
                    mc_chroma_win(&src, ss, fx, fy, bw, bh, &mut a, 16);
                    chroma_scalar(&src, ss, fx, fy, bw, bh, &mut b, 16);
                    assert_eq!(a, b, "chroma {bw}x{bh} frac {fx},{fy}");
                }
            }
        }
    }

    #[test]
    fn weighted_store_simd_matches_scalar() {
        let mut seed = 0xc2b2_ae35u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed
        };
        let weights = [
            Weight::Default,
            Weight::Weighted { log_wd: 5, w0: 16, w1: 32, o0: 3, o1: -2 },
            Weight::Weighted { log_wd: 6, w0: -20, w1: 50, o0: 10, o1: -7 },
            Weight::Weighted { log_wd: 0, w0: 1, w1: 1, o0: 0, o1: 0 },
        ];
        for &(bw, bh) in &[(4, 4), (8, 8), (16, 16), (16, 8), (8, 4), (5, 3), (9, 1)] {
            let n = bw.max(16) * bh;
            let a: Vec<u8> = (0..n).map(|_| next() as u8).collect();
            let b: Vec<u8> = (0..n).map(|_| next() as u8).collect();
            let ps = bw.max(16);
            for w in weights {
                for pair in [true, false] {
                    let mut simd = vec![0u8; n];
                    let mut scalar = vec![0u8; n];
                    let p1 = if pair { Some(b.as_slice()) } else { None };
                    weighted_store(&mut simd, ps, Some(&a), p1, ps, bw, bh, w);
                    weighted_store_scalar(&mut scalar, ps, Some(&a), p1, ps, bw, bh, w);
                    assert_eq!(simd, scalar, "weighted {bw}x{bh} pair={pair} {w:?}");
                }
            }
        }
    }
}
