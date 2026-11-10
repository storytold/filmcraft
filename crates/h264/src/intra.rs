//! Intra prediction (8.3): Intra_4x4, Intra_8x8 (with reference filtering), Intra_16x16 and chroma
//! (4:2:0 and 4:2:2), for any sample type / bit depth.

use crate::picture::{Format, Sample};

/// Neighbouring sample availability for one block.
#[derive(Clone, Copy, Debug, Default)]
pub struct Avail {
    pub left: bool,
    pub top: bool,
    pub top_left: bool,
    pub top_right: bool,
}

/// Neighbouring samples of an NxN block in one line: p[-1, y] at `O - 1 - y` (y = 0..N-1),
/// p[-1, -1] at `O`, p[x, -1] at `O + 1 + x` (x = 0..2N-1). The sample past each end repeats
/// the last one, so the end cases of the three-tap formulas need no special handling.
type Edge<S> = [S; 32];
const O: usize = 9;

/// The edge line of the `N` x `N` block at (`x`, `y`): unavailable samples are `mid`
/// (1 << (BitDepth - 1)); top-right samples are read only when `a.top_right`.
fn load_edge<S: Sample, const N: usize>(plane: &[S], stride: usize, x: usize, y: usize, a: Avail, mid: S) -> Edge<S> {
    let mut e = [mid; 32];
    if a.top {
        let row = &plane[(y - 1) * stride + x..];
        if a.top_right {
            e[O + 1..O + 1 + 2 * N].copy_from_slice(&row[..2 * N]);
        } else {
            e[O + 1..O + 1 + N].copy_from_slice(&row[..N]);
            let v = row[N - 1];
            e[O + 1 + N..O + 1 + 2 * N].fill(v);
        }
        e[O + 1 + 2 * N] = e[O + 2 * N];
    }
    if a.left {
        let col = &plane[y * stride + x - 1..];
        for i in 0..N {
            e[O - 1 - i] = col[i * stride];
        }
        e[O - 1 - N] = e[O - N];
    }
    if a.top_left {
        e[O] = plane[(y - 1) * stride + x - 1];
    }
    e
}

#[inline(always)]
fn row_mut<S, const N: usize>(out: &mut [S], stride: usize, y: usize) -> &mut [S] {
    &mut out[y * stride..y * stride + N]
}

/// The three-tap average (e[c - 1] + 2 e[c] + e[c + 1] + 2) >> 2 along the edge line.
#[inline(always)]
fn tap3<S: Sample>(e: &Edge<S>, c: usize, max: i32) -> S {
    S::clip((e[c - 1].to_i32() + 2 * e[c].to_i32() + e[c + 1].to_i32() + 2) >> 2, max)
}

/// Directional / DC prediction shared by Intra_4x4 and Intra_8x8 (8.3.1.2.x, 8.3.2.2.x). Every
/// directional sample is a two-tap (`f2`) or three-tap (`f3`) average along the edge line.
fn pred_nxn<S: Sample, const N: usize>(mode: u8, e: &Edge<S>, a: Avail, out: &mut [S], stride: usize, fmt: Format) {
    let max = fmt.max();
    match mode {
        0 => {
            for y in 0..N {
                row_mut::<S, N>(out, stride, y).copy_from_slice(&e[O + 1..O + 1 + N]);
            }
        }
        1 => {
            for y in 0..N {
                row_mut::<S, N>(out, stride, y).fill(e[O - 1 - y]);
            }
        }
        2 => {
            let n = N as i32;
            let shift = if N == 4 { 2 } else { 3 };
            let st: i32 = e[O + 1..O + 1 + N].iter().map(|v| v.to_i32()).sum();
            let sl: i32 = e[O - N..O].iter().map(|v| v.to_i32()).sum();
            let dc = match (a.top, a.left) {
                (true, true) => (st + sl + n) >> (shift + 1),
                (false, true) => (sl + (n >> 1)) >> shift,
                (true, false) => (st + (n >> 1)) >> shift,
                _ => fmt.mid(),
            };
            let dc = S::clip(dc, max);
            for y in 0..N {
                row_mut::<S, N>(out, stride, y).fill(dc);
            }
        }
        _ => {
            let mut f2 = [S::default(); 32];
            let mut f3 = [S::default(); 32];
            for c in 1..31 {
                f2[c] = S::avg(e[c], e[c + 1]);
                f3[c] = tap3(e, c, max);
            }
            match mode {
                3 => {
                    for y in 0..N {
                        row_mut::<S, N>(out, stride, y).copy_from_slice(&f3[O + 2 + y..O + 2 + y + N]);
                    }
                }
                4 => {
                    for y in 0..N {
                        row_mut::<S, N>(out, stride, y).copy_from_slice(&f3[O - y..O - y + N]);
                    }
                }
                5 => {
                    for y in 0..N {
                        let r = row_mut::<S, N>(out, stride, y);
                        for x in 0..N {
                            let z = 2 * x as isize - y as isize;
                            let i = O + x - (y >> 1);
                            r[x] = if z < 0 {
                                f3[(O as isize + 1 + z) as usize]
                            } else if z % 2 == 0 {
                                f2[i]
                            } else {
                                f3[i]
                            };
                        }
                    }
                }
                6 => {
                    for y in 0..N {
                        let r = row_mut::<S, N>(out, stride, y);
                        for x in 0..N {
                            let z = 2 * y as isize - x as isize;
                            let i = O + (x >> 1) - y;
                            r[x] = if z < 0 {
                                f3[(O as isize - 1 - z) as usize]
                            } else if z % 2 == 0 {
                                f2[i - 1]
                            } else {
                                f3[i]
                            };
                        }
                    }
                }
                7 => {
                    for y in 0..N {
                        let s = if y % 2 == 0 { &f2[O + 1 + (y >> 1)..] } else { &f3[O + 2 + (y >> 1)..] };
                        row_mut::<S, N>(out, stride, y).copy_from_slice(&s[..N]);
                    }
                }
                _ => {
                    for y in 0..N {
                        let r = row_mut::<S, N>(out, stride, y);
                        for x in 0..N {
                            let z = x + 2 * y;
                            r[x] = if z > 2 * N - 3 {
                                e[O - N]
                            } else {
                                let i = O - 2 - y - (x >> 1);
                                if z % 2 == 0 { f2[i] } else { f3[i] }
                            };
                        }
                    }
                }
            }
        }
    }
}

/// Intra_4x4 prediction written into `plane` at (x, y).
pub fn pred4x4<S: Sample>(plane: &mut [S], stride: usize, x: usize, y: usize, mode: u8, a: Avail, fmt: Format) {
    let e = load_edge::<S, 4>(plane, stride, x, y, a, S::clip(fmt.mid(), fmt.max()));
    pred_nxn::<S, 4>(mode, &e, a, &mut plane[y * stride + x..], stride, fmt);
}

/// Intra_8x8 prediction (with reference sample filtering) written into `plane` at (x, y).
pub fn pred8x8<S: Sample>(plane: &mut [S], stride: usize, x: usize, y: usize, mode: u8, a: Avail, fmt: Format) {
    let max = fmt.max();
    let p = load_edge::<S, 8>(plane, stride, x, y, a, S::clip(fmt.mid(), max));
    let mut f = p;
    let (tl, t0, t1, l0, l1) = (p[O].to_i32(), p[O + 1].to_i32(), p[O + 2].to_i32(), p[O - 1].to_i32(), p[O - 2].to_i32());
    if a.top {
        for c in O + 1..O + 17 {
            f[c] = tap3(&p, c, max);
        }
        if !a.top_left {
            f[O + 1] = S::clip((3 * t0 + t1 + 2) >> 2, max);
        }
        f[O + 17] = f[O + 16];
    }
    if a.top_left {
        f[O] = match (a.top, a.left) {
            (true, true) => tap3(&p, O, max),
            (true, false) => S::clip((3 * tl + t0 + 2) >> 2, max),
            (false, true) => S::clip((3 * tl + l0 + 2) >> 2, max),
            _ => p[O],
        };
    }
    if a.left {
        for c in O - 8..O {
            f[c] = tap3(&p, c, max);
        }
        if !a.top_left {
            f[O - 1] = S::clip((3 * l0 + l1 + 2) >> 2, max);
        }
        f[O - 9] = f[O - 8];
    }
    pred_nxn::<S, 8>(mode, &f, a, &mut plane[y * stride + x..], stride, fmt);
}

/// Intra_16x16 prediction (8.3.3) for the macroblock at luma (x, y).
pub fn pred16x16<S: Sample>(plane: &mut [S], stride: usize, x: usize, y: usize, mode: u8, a: Avail, fmt: Format) {
    let max = fmt.max();
    let mut top = [0i32; 16];
    let mut left = [0i32; 16];
    if a.top {
        for i in 0..16 {
            top[i] = plane[(y - 1) * stride + x + i].to_i32();
        }
    }
    if a.left {
        for i in 0..16 {
            left[i] = plane[(y + i) * stride + x - 1].to_i32();
        }
    }
    let base = y * stride + x;
    match mode {
        0 => {
            let t: [S; 16] = top.map(|v| S::clip(v, max));
            for r in 0..16 {
                row_mut::<S, 16>(&mut plane[base..], stride, r).copy_from_slice(&t);
            }
        }
        1 => {
            for r in 0..16 {
                plane[base + r * stride..base + r * stride + 16].fill(S::clip(left[r], max));
            }
        }
        2 => {
            let st: i32 = top.iter().sum();
            let sl: i32 = left.iter().sum();
            let dc = match (a.top, a.left) {
                (true, true) => (st + sl + 16) >> 5,
                (false, true) => (sl + 8) >> 4,
                (true, false) => (st + 8) >> 4,
                _ => fmt.mid(),
            };
            let dc = S::clip(dc, max);
            for r in 0..16 {
                plane[base + r * stride..base + r * stride + 16].fill(dc);
            }
        }
        _ => {
            let tl = if a.top_left { plane[(y - 1) * stride + x - 1].to_i32() } else { fmt.mid() };
            let t = |i: i32| if i < 0 { tl } else { top[i as usize] };
            let l = |i: i32| if i < 0 { tl } else { left[i as usize] };
            let mut h = 0;
            let mut v = 0;
            for k in 0..8 {
                h += (k + 1) * (t(8 + k) - t(6 - k));
                v += (k + 1) * (l(8 + k) - l(6 - k));
            }
            let aa = 16 * (left[15] + top[15]);
            let b = (5 * h + 32) >> 6;
            let c = (5 * v + 32) >> 6;
            for r in 0..16 {
                let row = row_mut::<S, 16>(&mut plane[base..], stride, r);
                for (cc, v) in row.iter_mut().enumerate() {
                    *v = S::clip((aa + b * (cc as i32 - 7) + c * (r as i32 - 7) + 16) >> 5, max);
                }
            }
        }
    }
}

/// Chroma intra prediction (8.3.4) for the 8 x `fmt.mb_hc` chroma block of a macroblock at
/// chroma (x, y). Modes: 0 DC, 1 horizontal, 2 vertical, 3 plane.
pub fn pred_chroma<S: Sample>(plane: &mut [S], stride: usize, x: usize, y: usize, mode: u8, a: Avail, fmt: Format) {
    const W: usize = 8;
    let h = fmt.mb_hc.min(16);
    let (max, mid) = (fmt.max(), fmt.mid());
    let mut top = [0i32; W];
    let mut left = [0i32; 16];
    if a.top {
        for i in 0..W {
            top[i] = plane[(y - 1) * stride + x + i].to_i32();
        }
    }
    if a.left {
        for i in 0..h {
            left[i] = plane[(y + i) * stride + x - 1].to_i32();
        }
    }
    let base = y * stride + x;
    match mode {
        0 => {
            for by in 0..h / 4 {
                for bx in 0..W / 4 {
                    let st: i32 = top[bx * 4..bx * 4 + 4].iter().sum();
                    let sl: i32 = left[by * 4..by * 4 + 4].iter().sum();
                    let dc = if (bx == 0 && by == 0) || (bx > 0 && by > 0) {
                        match (a.top, a.left) {
                            (true, true) => (st + sl + 4) >> 3,
                            (false, true) => (sl + 2) >> 2,
                            (true, false) => (st + 2) >> 2,
                            _ => mid,
                        }
                    } else if bx > 0 {
                        if a.top {
                            (st + 2) >> 2
                        } else if a.left {
                            (sl + 2) >> 2
                        } else {
                            mid
                        }
                    } else if a.left {
                        (sl + 2) >> 2
                    } else if a.top {
                        (st + 2) >> 2
                    } else {
                        mid
                    };
                    for r in 0..4 {
                        let o = base + (by * 4 + r) * stride + bx * 4;
                        plane[o..o + 4].fill(S::clip(dc, max));
                    }
                }
            }
        }
        1 => {
            for r in 0..h {
                plane[base + r * stride..base + r * stride + W].fill(S::clip(left[r], max));
            }
        }
        2 => {
            let t: [S; W] = top.map(|v| S::clip(v, max));
            for r in 0..h {
                row_mut::<S, W>(&mut plane[base..], stride, r).copy_from_slice(&t);
            }
        }
        _ => {
            let tl = if a.top_left { plane[(y - 1) * stride + x - 1].to_i32() } else { fmt.mid() };
            let t = |i: i32| if i < 0 { tl } else { top[i as usize] };
            let l = |i: i32| if i < 0 { tl } else { left[i as usize] };
            // yCF = 4 for 4:2:2 (8-138..8-141)
            let ycf = (h as i32 - 8) / 2;
            let mut hh = 0;
            let mut v = 0;
            for k in 0..4 {
                hh += (k + 1) * (t(4 + k) - t(2 - k));
            }
            for k in 0..4 + ycf {
                v += (k + 1) * (l(4 + ycf + k) - l(2 + ycf - k));
            }
            let aa = 16 * (left[h - 1] + top[W - 1]);
            let b = (34 * hh + 32) >> 6;
            let c = ((34 - 29 * (h == 16) as i32) * v + 32) >> 6;
            for r in 0..h {
                let row = row_mut::<S, W>(&mut plane[base..], stride, r);
                for (cc, v) in row.iter_mut().enumerate() {
                    *v = S::clip((aa + b * (cc as i32 - 3) + c * (r as i32 - 3 - ycf) + 16) >> 5, max);
                }
            }
        }
    }
}

/// The per-sample spec formulas that [`pred_nxn`] replaces, for comparison in tests.
#[cfg(test)]
mod reference {
    use super::Avail;
    use crate::picture::{Format, Sample};

    /// Neighbouring samples of an NxN block: p[x,-1] for x = 0..2N-1, p[-1,y] for y = 0..N-1, p[-1,-1].
    #[derive(Clone, Copy)]
    struct Edge {
        top: [i32; 16],
        left: [i32; 8],
        tl: i32,
    }

    impl Edge {
        /// p[x, -1] for x >= -1
        #[inline(always)]
        fn t(&self, x: i32) -> i32 {
            if x < 0 { self.tl } else { self.top[x as usize] }
        }
        /// p[-1, y] for y >= -1
        #[inline(always)]
        fn l(&self, y: i32) -> i32 {
            if y < 0 { self.tl } else { self.left[y as usize] }
        }
    }

    fn load_edge<S: Sample>(plane: &[S], stride: usize, x: usize, y: usize, n: usize, a: Avail, mid: i32) -> Edge {
        let mut e = Edge { top: [mid; 16], left: [mid; 8], tl: mid };
        if a.top {
            let row = &plane[(y - 1) * stride + x..];
            for i in 0..n {
                e.top[i] = row[i].to_i32();
            }
            if a.top_right {
                for i in n..2 * n {
                    e.top[i] = row[i].to_i32();
                }
            } else {
                let v = e.top[n - 1];
                for t in &mut e.top[n..2 * n] {
                    *t = v;
                }
            }
        }
        if a.left {
            for i in 0..n {
                e.left[i] = plane[(y + i) * stride + x - 1].to_i32();
            }
        }
        if a.top_left {
            e.tl = plane[(y - 1) * stride + x - 1].to_i32();
        }
        e
    }

    /// Fill an `n`x`n` block at `out` with `f(x, y)`.
    fn fill_nxn<S: Sample>(n: i32, out: &mut [S], stride: usize, max: i32, f: impl Fn(i32, i32) -> i32) {
        for y in 0..n {
            for x in 0..n {
                out[y as usize * stride + x as usize] = S::clip(f(x, y), max);
            }
        }
    }

    /// DC value of Intra_NxN prediction (mode 2).
    fn dc_nxn(e: &Edge, n: i32, a: Avail, mid: i32) -> i32 {
        let shift = if n == 4 { 2 } else { 3 };
        let st: i32 = (0..n).map(|x| e.t(x)).sum();
        let sl: i32 = (0..n).map(|y| e.l(y)).sum();
        match (a.top, a.left) {
            (true, true) => (st + sl + n) >> (shift + 1),
            (false, true) => (sl + (n >> 1)) >> shift,
            (true, false) => (st + (n >> 1)) >> shift,
            _ => mid,
        }
    }

    /// Diagonal down-left sample (mode 3).
    fn diag_down_left(e: &Edge, n: i32, x: i32, y: i32) -> i32 {
        if x == n - 1 && y == n - 1 { (e.t(2 * n - 2) + 3 * e.t(2 * n - 1) + 2) >> 2 } else { (e.t(x + y) + 2 * e.t(x + y + 1) + e.t(x + y + 2) + 2) >> 2 }
    }

    /// Diagonal down-right sample (mode 4).
    fn diag_down_right(e: &Edge, x: i32, y: i32) -> i32 {
        if x > y {
            (e.t(x - y - 2) + 2 * e.t(x - y - 1) + e.t(x - y) + 2) >> 2
        } else if x < y {
            (e.l(y - x - 2) + 2 * e.l(y - x - 1) + e.l(y - x) + 2) >> 2
        } else {
            (e.t(0) + 2 * e.tl + e.l(0) + 2) >> 2
        }
    }

    /// Vertical-right sample (mode 5).
    fn vertical_right(e: &Edge, x: i32, y: i32) -> i32 {
        let z = 2 * x - y;
        if z >= 0 && z % 2 == 0 {
            (e.t(x - (y >> 1) - 1) + e.t(x - (y >> 1)) + 1) >> 1
        } else if z > 0 {
            (e.t(x - (y >> 1) - 2) + 2 * e.t(x - (y >> 1) - 1) + e.t(x - (y >> 1)) + 2) >> 2
        } else if z == -1 {
            (e.l(0) + 2 * e.tl + e.t(0) + 2) >> 2
        } else {
            (e.l(y - 2 * x - 1) + 2 * e.l(y - 2 * x - 2) + e.l(y - 2 * x - 3) + 2) >> 2
        }
    }

    /// Horizontal-down sample (mode 6).
    fn horizontal_down(e: &Edge, x: i32, y: i32) -> i32 {
        let z = 2 * y - x;
        if z >= 0 && z % 2 == 0 {
            (e.l(y - (x >> 1) - 1) + e.l(y - (x >> 1)) + 1) >> 1
        } else if z > 0 {
            (e.l(y - (x >> 1) - 2) + 2 * e.l(y - (x >> 1) - 1) + e.l(y - (x >> 1)) + 2) >> 2
        } else if z == -1 {
            (e.l(0) + 2 * e.tl + e.t(0) + 2) >> 2
        } else {
            (e.t(x - 2 * y - 1) + 2 * e.t(x - 2 * y - 2) + e.t(x - 2 * y - 3) + 2) >> 2
        }
    }

    /// Vertical-left sample (mode 7).
    fn vertical_left(e: &Edge, x: i32, y: i32) -> i32 {
        let i = x + (y >> 1);
        if y % 2 == 0 { (e.t(i) + e.t(i + 1) + 1) >> 1 } else { (e.t(i) + 2 * e.t(i + 1) + e.t(i + 2) + 2) >> 2 }
    }

    /// Horizontal-up sample (mode 8).
    fn horizontal_up(e: &Edge, n: i32, x: i32, y: i32) -> i32 {
        let zmax = 2 * n - 3;
        let z = x + 2 * y;
        let i = y + (x >> 1);
        if z < zmax && z % 2 == 0 {
            (e.l(i) + e.l(i + 1) + 1) >> 1
        } else if z < zmax {
            (e.l(i) + 2 * e.l(i + 1) + e.l(i + 2) + 2) >> 2
        } else if z == zmax {
            (e.l(n - 2) + 3 * e.l(n - 1) + 2) >> 2
        } else {
            e.l(n - 1)
        }
    }

    /// Directional / DC prediction shared by Intra_4x4 and Intra_8x8 (8.3.1.2.x, 8.3.2.2.x).
    fn pred_nxn<S: Sample>(mode: u8, e: &Edge, n: i32, a: Avail, out: &mut [S], stride: usize, fmt: Format) {
        let max = fmt.max();
        match mode {
            0 => fill_nxn(n, out, stride, max, |x, _| e.t(x)),
            1 => fill_nxn(n, out, stride, max, |_, y| e.l(y)),
            2 => {
                let dc = dc_nxn(e, n, a, fmt.mid());
                fill_nxn(n, out, stride, max, |_, _| dc);
            }
            3 => fill_nxn(n, out, stride, max, |x, y| diag_down_left(e, n, x, y)),
            4 => fill_nxn(n, out, stride, max, |x, y| diag_down_right(e, x, y)),
            5 => fill_nxn(n, out, stride, max, |x, y| vertical_right(e, x, y)),
            6 => fill_nxn(n, out, stride, max, |x, y| horizontal_down(e, x, y)),
            7 => fill_nxn(n, out, stride, max, |x, y| vertical_left(e, x, y)),
            _ => fill_nxn(n, out, stride, max, |x, y| horizontal_up(e, n, x, y)),
        }
    }

    /// Intra_4x4 prediction written into `plane` at (x, y).
    pub fn pred4x4<S: Sample>(plane: &mut [S], stride: usize, x: usize, y: usize, mode: u8, a: Avail, fmt: Format) {
        let e = load_edge(plane, stride, x, y, 4, a, fmt.mid());
        pred_nxn(mode, &e, 4, a, &mut plane[y * stride + x..], stride, fmt);
    }

    /// Intra_8x8 prediction (with reference sample filtering) written into `plane` at (x, y).
    pub fn pred8x8<S: Sample>(plane: &mut [S], stride: usize, x: usize, y: usize, mode: u8, a: Avail, fmt: Format) {
        let p = load_edge(plane, stride, x, y, 8, a, fmt.mid());
        let mut f = p;
        if a.top {
            f.top[0] = if a.top_left { (p.tl + 2 * p.top[0] + p.top[1] + 2) >> 2 } else { (3 * p.top[0] + p.top[1] + 2) >> 2 };
            for i in 1..15 {
                f.top[i] = (p.top[i - 1] + 2 * p.top[i] + p.top[i + 1] + 2) >> 2;
            }
            f.top[15] = (p.top[14] + 3 * p.top[15] + 2) >> 2;
        }
        if a.top_left {
            f.tl = match (a.top, a.left) {
                (true, true) => (p.top[0] + 2 * p.tl + p.left[0] + 2) >> 2,
                (true, false) => (3 * p.tl + p.top[0] + 2) >> 2,
                (false, true) => (3 * p.tl + p.left[0] + 2) >> 2,
                _ => p.tl,
            };
        }
        if a.left {
            f.left[0] = if a.top_left { (p.tl + 2 * p.left[0] + p.left[1] + 2) >> 2 } else { (3 * p.left[0] + p.left[1] + 2) >> 2 };
            for i in 1..7 {
                f.left[i] = (p.left[i - 1] + 2 * p.left[i] + p.left[i + 1] + 2) >> 2;
            }
            f.left[7] = (p.left[6] + 3 * p.left[7] + 2) >> 2;
        }
        pred_nxn(mode, &f, 8, a, &mut plane[y * stride + x..], stride, fmt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Vec<u8>, usize) {
        // 12x12 plane; block at (4,4); top row y=3 = 10,20,..; left column x=3 = 1,2,3,...
        let stride = 16;
        let mut p = vec![0u8; stride * 16];
        for i in 0..12 {
            p[3 * stride + 4 + i] = (10 * (i + 1)) as u8;
        }
        for i in 0..8 {
            p[(4 + i) * stride + 3] = (i + 1) as u8;
        }
        p[3 * stride + 3] = 5;
        (p, stride)
    }
    const ALL: Avail = Avail { left: true, top: true, top_left: true, top_right: true };
    const F: Format = Format::YUV420_8;

    /// Intra_4x4 and Intra_8x8 match the per-sample spec formulas for every mode and every
    /// neighbour availability on random samples of `fmt`'s bit depth, stored as `S`.
    fn nxn_matches_reference_for<S: Sample>(fmt: Format) {
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let s = 32;
        for iter in 0..4000 {
            let base: Vec<S> = (0..s * 32).map(|_| S::clip((next() & fmt.max() as u64) as i32, fmt.max())).collect();
            let bits = iter % 16;
            let a = Avail { left: bits & 1 != 0, top: bits & 2 != 0, top_left: bits & 4 != 0, top_right: bits & 8 != 0 };
            for mode in 0..9 {
                let (mut got, mut want) = (base.clone(), base.clone());
                if iter % 2 == 0 {
                    pred4x4(&mut got, s, 8, 8, mode, a, fmt);
                    reference::pred4x4(&mut want, s, 8, 8, mode, a, fmt);
                } else {
                    pred8x8(&mut got, s, 8, 8, mode, a, fmt);
                    reference::pred8x8(&mut want, s, 8, 8, mode, a, fmt);
                }
                assert_eq!(got, want, "iteration {iter}: mode {mode} {a:?}");
            }
        }
    }

    #[test]
    fn nxn_matches_reference() {
        nxn_matches_reference_for::<u8>(F);
    }

    /// The same for 9- and 10-bit samples in 16-bit storage (High 10, High 4:2:2).
    #[test]
    fn nxn_matches_reference_at_high_bit_depths() {
        nxn_matches_reference_for::<u16>(Format { bit_depth: 9, mb_hc: 8 });
        nxn_matches_reference_for::<u16>(Format { bit_depth: 10, mb_hc: 16 });
    }

    #[test]
    fn i4_vertical_horizontal_dc() {
        let (mut p, s) = setup();
        pred4x4(&mut p, s, 4, 4, 0, ALL, F);
        assert_eq!(&p[4 * s + 4..4 * s + 8], &[10, 20, 30, 40]);
        assert_eq!(&p[7 * s + 4..7 * s + 8], &[10, 20, 30, 40]);
        pred4x4(&mut p, s, 4, 4, 1, ALL, F);
        assert_eq!(&p[5 * s + 4..5 * s + 8], &[2, 2, 2, 2]);
        pred4x4(&mut p, s, 4, 4, 2, ALL, F);
        // (10+20+30+40 + 1+2+3+4 + 4) >> 3 = 114 >> 3 = 14
        assert_eq!(p[4 * s + 4], 14);
    }

    #[test]
    fn i4_diag_down_left_hand_computed() {
        let (mut p, s) = setup();
        pred4x4(&mut p, s, 4, 4, 3, ALL, F);
        // pred[0,0] = (t0 + 2 t1 + t2 + 2) >> 2 = (10 + 40 + 30 + 2) >> 2 = 20
        assert_eq!(p[4 * s + 4], 20);
        // pred[3,3] = (t6 + 3 t7 + 2) >> 2 = (70 + 240 + 2) >> 2 = 78
        assert_eq!(p[7 * s + 7], 78);
    }

    #[test]
    fn i4_diag_down_right_hand_computed() {
        let (mut p, s) = setup();
        pred4x4(&mut p, s, 4, 4, 4, ALL, F);
        // x==y: (t0 + 2 tl + l0 + 2) >> 2 = (10 + 10 + 1 + 2) >> 2 = 5
        assert_eq!(p[4 * s + 4], 5);
        // x=1,y=0: (tl + 2 t0 + t1 + 2)>>2 = (5 + 20 + 20 + 2) >> 2 = 11
        assert_eq!(p[4 * s + 5], 11);
        // x=0,y=1: (tl + 2 l0 + l1 + 2) >> 2 = (5 + 2 + 2 + 2) >> 2 = 2
        assert_eq!(p[5 * s + 4], 2);
    }

    #[test]
    fn i4_top_right_substitution() {
        let (mut p, s) = setup();
        let a = Avail { top_right: false, ..ALL };
        pred4x4(&mut p, s, 4, 4, 3, a, F);
        // top-right replaced by t3 = 40: pred[3,3] = (40 + 120 + 2) >> 2 = 40
        assert_eq!(p[7 * s + 7], 40);
    }

    #[test]
    fn i16_plane_flat_is_flat() {
        let s = 32;
        let mut p = vec![77u8; s * 32];
        pred16x16(&mut p, s, 8, 8, 3, ALL, F);
        assert!(p[8 * s + 8..8 * s + 24].iter().all(|&v| v == 77));
    }

    #[test]
    fn chroma_dc_rules() {
        let s = 16;
        let mut p = vec![0u8; s * 16];
        for i in 0..8 {
            p[3 * s + 4 + i] = if i < 4 { 40 } else { 80 };
            p[(4 + i) * s + 3] = if i < 4 { 8 } else { 16 };
        }
        pred_chroma(&mut p, s, 4, 4, 0, ALL, F);
        assert_eq!(p[4 * s + 4], (160 + 32 + 4) >> 3); // top-left block: both
        assert_eq!(p[4 * s + 8], 80); // top-right block: top only
        assert_eq!(p[8 * s + 4], 16); // bottom-left: left only
        assert_eq!(p[8 * s + 8], ((320 + 64 + 4) >> 3) as u8); // bottom-right: both
    }
}
