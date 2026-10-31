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

#[cfg(test)]
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

#[cfg(test)]
#[inline(always)]
fn tap6(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
    a - 5 * b + 20 * c + 20 * d - 5 * e + f
}

#[cfg(test)]
#[inline(always)]
fn clip1(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[cfg(test)]
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

/// Luma interpolation from a window whose rows are `ss` apart in `src` (`src[0]` is the sample at
/// (x - 2, y - 2)), put into `out`: the tests' entry to the decoder's kernel.
#[cfg(test)]
pub fn mc_luma_win(src: &[u8], ss: usize, blk: McBlock, out: &mut [u8], os: usize) {
    let McBlock { fx, fy, bw, bh } = blk;
    filmcraft_simd::h264_luma((src, ss), (fx, fy), (bw, bh), (out, os), filmcraft_simd::H264Out::Put);
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

/// Chroma interpolation from a window whose rows are `ss` apart in `src`, put into `out` (tests).
#[cfg(test)]
pub fn mc_chroma_win(src: &[u8], ss: usize, blk: McBlock, out: &mut [u8], os: usize) {
    let McBlock { fx, fy, bw, bh } = blk;
    filmcraft_simd::h264_chroma((src, ss), (fx, fy), (bw, bh), (out, os), filmcraft_simd::H264Out::Put);
}

/// Fractional offset and size of one motion-compensated block.
#[cfg(test)]
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

/// Combine prediction(s) into `dst` (stride `ds`). `p0`/`p1` have stride `ps`: the reference for
/// the prediction kernels' output modes (`filmcraft_simd::H264Out`).
#[cfg(test)]
pub fn weighted_store(dst: &mut [u8], ds: usize, p0: Option<&[u8]>, p1: Option<&[u8]>, ps: usize, (bw, bh): (usize, usize), w: Weight) {
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
    fn weighting() {
        let a = [100u8; 4];
        let b = [50u8; 4];
        let mut d = [0u8; 4];
        weighted_store(&mut d, 4, Some(&a), Some(&b), 4, (4, 1), Weight::Default);
        assert_eq!(d, [75; 4]);
        weighted_store(&mut d, 4, Some(&a), None, 4, (4, 1), Weight::Weighted { log_wd: 5, w0: 16, w1: 0, o0: 3, o1: 0 });
        assert_eq!(d, [53; 4]);
    }
}
