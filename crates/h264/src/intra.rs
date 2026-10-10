//! Intra prediction (8.3): Intra_4x4, Intra_8x8 (with reference filtering), Intra_16x16 and chroma.
//!
//! Samples are `u16` for every bit depth; `max` is the Clip1Y / Clip1C ceiling (255 for 8-bit
//! streams, `(1 << bit_depth) - 1` otherwise), so results at `max == 255` are bit-identical to the
//! original 8-bit decoder. Spec: Rec. ITU-T H.264 (04/2017), clauses 8.3.1–8.3.4; the "no
//! neighbours" DC values are `1 << (BitDepthY − 1)` / `1 << (BitDepthC − 1)` (8-51, 8-94, 8-121,
//! 8-135/8-138/8-141).
//!
//! Chroma predicts the whole macroblock block of one component: 8x8 for ChromaArrayType == 1
//! (4:2:0) and 8x16 for ChromaArrayType == 2 (4:2:2) (8.3.4). Its DC mode stays per 4x4 sub-block
//! (8.3.4.1): chroma4x4BlkIdx runs 0..7 in 4:2:2 and each sub-block averages its own 4 top and 4
//! left reference samples under the same three availability cases.
//!
//! Every call validates its geometry once up front ([`fits`]); a malformed call (block or required
//! neighbours outside the plane) predicts nothing instead of panicking.

/// Neighbouring sample availability for one block.
#[derive(Clone, Copy, Debug, Default)]
pub struct Avail {
    pub left: bool,
    pub top: bool,
    pub top_left: bool,
    pub top_right: bool,
}

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
        if x < 0 { self.tl } else { self.top.get(x as usize).copied().unwrap_or(self.tl) }
    }
    /// p[-1, y] for y >= -1
    #[inline(always)]
    fn l(&self, y: i32) -> i32 {
        if y < 0 { self.tl } else { self.left.get(y as usize).copied().unwrap_or(self.tl) }
    }
}

/// True when every sample the prediction of a `w` x `h` block at (x, y) may touch is inside the
/// plane: the block itself, `top_w` top samples (top row, right edge included) when `a.top`, the
/// `h` left samples when `a.left`, and p[-1,-1] when `a.top_left`. Indices may legally span across
/// row ends (planes are flat and the availability flags keep the right picture edge from reading
/// top-right), so this bounds-checks the flat indices, once, before the hot loops.
#[allow(clippy::too_many_arguments)]
fn fits(plane: &[u16], stride: usize, x: usize, y: usize, w: usize, h: usize, top_w: usize, a: Avail) -> bool {
    let len = plane.len();
    if stride == 0 {
        return false;
    }
    // The block: largest index is its bottom-right sample.
    let Some(base) = y.checked_mul(stride).and_then(|v| v.checked_add(x)) else {
        return false;
    };
    let Some(off) = h.saturating_sub(1).checked_mul(stride).and_then(|v| v.checked_add(w.saturating_sub(1))) else {
        return false;
    };
    let Some(last) = base.checked_add(off) else {
        return false;
    };
    if last >= len {
        return false;
    }
    if a.top {
        // Top row p[x .. x + top_w - 1, -1].
        let Some(t) =
            y.checked_sub(1).and_then(|yb| yb.checked_mul(stride)).and_then(|v| v.checked_add(x)).and_then(|v| v.checked_add(top_w.saturating_sub(1)))
        else {
            return false;
        };
        if t >= len {
            return false;
        }
    }
    if a.top_left {
        // p[-1, -1].
        let Some(t) = y.checked_sub(1).and_then(|yb| yb.checked_mul(stride)).and_then(|v| v.checked_add(x)) else {
            return false;
        };
        if x == 0 || t == 0 || t > len {
            return false;
        }
    }
    if a.left {
        // Left column p[-1, y .. y + h - 1]: the deepest sample bounds the column.
        if x == 0 {
            return false;
        }
        let Some(l) = y.checked_add(h.saturating_sub(1)).and_then(|yb| yb.checked_mul(stride)).and_then(|v| v.checked_add(x - 1)) else {
            return false;
        };
        if l >= len {
            return false;
        }
    }
    true
}

fn load_edge(plane: &[u16], stride: usize, x: usize, y: usize, n: usize, a: Avail, mid: i32) -> Edge {
    let mut e = Edge { top: [mid; 16], left: [mid; 8], tl: mid };
    if a.top {
        let base = (y - 1) * stride + x;
        for i in 0..n {
            e.top[i] = i32::from(plane[base + i]);
        }
        if a.top_right {
            for i in n..2 * n {
                e.top[i] = i32::from(plane[base + i]);
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
            e.left[i] = i32::from(plane[(y + i) * stride + x - 1]);
        }
    }
    if a.top_left {
        e.tl = i32::from(plane[(y - 1) * stride + x - 1]);
    }
    e
}

/// Directional / DC prediction shared by Intra_4x4 and Intra_8x8 (8.3.1.2.x, 8.3.2.2.x).
fn pred_nxn(mode: u8, e: &Edge, n: i32, a: Avail, out: &mut [u16], stride: usize, max: i32) {
    let mid = mid(max);
    let mut put = |x: i32, y: i32, v: i32| out[y as usize * stride + x as usize] = clip(v, max);
    match mode {
        0 => {
            for y in 0..n {
                for x in 0..n {
                    put(x, y, e.t(x));
                }
            }
        }
        1 => {
            for y in 0..n {
                for x in 0..n {
                    put(x, y, e.l(y));
                }
            }
        }
        2 => {
            let shift = if n == 4 { 2 } else { 3 };
            let st: i32 = (0..n).map(|x| e.t(x)).sum();
            let sl: i32 = (0..n).map(|y| e.l(y)).sum();
            let dc = match (a.top, a.left) {
                (true, true) => (st + sl + n) >> (shift + 1),
                (false, true) => (sl + (n >> 1)) >> shift,
                (true, false) => (st + (n >> 1)) >> shift,
                _ => mid,
            };
            for y in 0..n {
                for x in 0..n {
                    put(x, y, dc);
                }
            }
        }
        3 => {
            for y in 0..n {
                for x in 0..n {
                    let v = if x == n - 1 && y == n - 1 {
                        (e.t(2 * n - 2) + 3 * e.t(2 * n - 1) + 2) >> 2
                    } else {
                        (e.t(x + y) + 2 * e.t(x + y + 1) + e.t(x + y + 2) + 2) >> 2
                    };
                    put(x, y, v);
                }
            }
        }
        4 => {
            for y in 0..n {
                for x in 0..n {
                    let v = if x > y {
                        (e.t(x - y - 2) + 2 * e.t(x - y - 1) + e.t(x - y) + 2) >> 2
                    } else if x < y {
                        (e.l(y - x - 2) + 2 * e.l(y - x - 1) + e.l(y - x) + 2) >> 2
                    } else {
                        (e.t(0) + 2 * e.tl + e.l(0) + 2) >> 2
                    };
                    put(x, y, v);
                }
            }
        }
        5 => {
            for y in 0..n {
                for x in 0..n {
                    let z = 2 * x - y;
                    let v = if z >= 0 && z % 2 == 0 {
                        (e.t(x - (y >> 1) - 1) + e.t(x - (y >> 1)) + 1) >> 1
                    } else if z > 0 {
                        (e.t(x - (y >> 1) - 2) + 2 * e.t(x - (y >> 1) - 1) + e.t(x - (y >> 1)) + 2) >> 2
                    } else if z == -1 {
                        (e.l(0) + 2 * e.tl + e.t(0) + 2) >> 2
                    } else {
                        (e.l(y - 2 * x - 1) + 2 * e.l(y - 2 * x - 2) + e.l(y - 2 * x - 3) + 2) >> 2
                    };
                    put(x, y, v);
                }
            }
        }
        6 => {
            for y in 0..n {
                for x in 0..n {
                    let z = 2 * y - x;
                    let v = if z >= 0 && z % 2 == 0 {
                        (e.l(y - (x >> 1) - 1) + e.l(y - (x >> 1)) + 1) >> 1
                    } else if z > 0 {
                        (e.l(y - (x >> 1) - 2) + 2 * e.l(y - (x >> 1) - 1) + e.l(y - (x >> 1)) + 2) >> 2
                    } else if z == -1 {
                        (e.l(0) + 2 * e.tl + e.t(0) + 2) >> 2
                    } else {
                        (e.t(x - 2 * y - 1) + 2 * e.t(x - 2 * y - 2) + e.t(x - 2 * y - 3) + 2) >> 2
                    };
                    put(x, y, v);
                }
            }
        }
        7 => {
            for y in 0..n {
                for x in 0..n {
                    let i = x + (y >> 1);
                    let v = if y % 2 == 0 { (e.t(i) + e.t(i + 1) + 1) >> 1 } else { (e.t(i) + 2 * e.t(i + 1) + e.t(i + 2) + 2) >> 2 };
                    put(x, y, v);
                }
            }
        }
        _ => {
            let zmax = 2 * n - 3;
            for y in 0..n {
                for x in 0..n {
                    let z = x + 2 * y;
                    let i = y + (x >> 1);
                    let v = if z < zmax && z % 2 == 0 {
                        (e.l(i) + e.l(i + 1) + 1) >> 1
                    } else if z < zmax {
                        (e.l(i) + 2 * e.l(i + 1) + e.l(i + 2) + 2) >> 2
                    } else if z == zmax {
                        (e.l(n - 2) + 3 * e.l(n - 1) + 2) >> 2
                    } else {
                        e.l(n - 1)
                    };
                    put(x, y, v);
                }
            }
        }
    }
}

/// Clip1Y / Clip1C (5-3, 5-4): clamp to 0..=max. Total on hostile `max` (never panics, never
/// wraps): the ceiling is clamped to the `u16` sample range first.
#[inline(always)]
fn clip(v: i32, max: i32) -> u16 {
    v.max(0).min(max.max(0).min(u16::MAX as i32)) as u16
}

/// Mid grey `1 << (BitDepth - 1)` of a sample range 0..=max: the value the spec uses where no
/// neighbouring sample is available (8-51, 8-94, 8-121, 8-135/8-138/8-141).
#[inline(always)]
fn mid(max: i32) -> i32 {
    (max.max(0).min(u16::MAX as i32) + 1) >> 1
}

/// Intra_4x4 prediction written into `plane` at (x, y).
pub fn pred4x4(plane: &mut [u16], stride: usize, x: usize, y: usize, mode: u8, a: Avail, max: i32) {
    if !fits(plane, stride, x, y, 4, 4, 8, a) {
        return;
    }
    let e = load_edge(plane, stride, x, y, 4, a, mid(max));
    pred_nxn(mode, &e, 4, a, &mut plane[y * stride + x..], stride, max);
}

/// Intra_8x8 prediction (with reference filtering) written into `plane` at (x, y).
pub fn pred8x8(plane: &mut [u16], stride: usize, x: usize, y: usize, mode: u8, a: Avail, max: i32) {
    if !fits(plane, stride, x, y, 8, 8, 16, a) {
        return;
    }
    let p = load_edge(plane, stride, x, y, 8, a, mid(max));
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
    pred_nxn(mode, &f, 8, a, &mut plane[y * stride + x..], stride, max);
}

/// Intra_16x16 prediction (8.3.3) for the macroblock at luma (x, y).
pub fn pred16x16(plane: &mut [u16], stride: usize, x: usize, y: usize, mode: u8, a: Avail, max: i32) {
    if !fits(plane, stride, x, y, 16, 16, 16, a) {
        return;
    }
    let mid = mid(max);
    // Unavailable edges stay 0 as in the original 8-bit code: they are read only in combinations
    // the spec forbids (e.g. Vertical with no top samples); the spec-defined fallbacks (DC with no
    // neighbours, p[-1,-1] in Plane) use `mid` below.
    let mut top = [0i32; 16];
    let mut left = [0i32; 16];
    if a.top {
        for (i, t) in top.iter_mut().enumerate() {
            *t = i32::from(plane[(y - 1) * stride + x + i]);
        }
    }
    if a.left {
        for (i, l) in left.iter_mut().enumerate() {
            *l = i32::from(plane[(y + i) * stride + x - 1]);
        }
    }
    let base = y * stride + x;
    match mode {
        0 => {
            for r in 0..16 {
                for c in 0..16 {
                    plane[base + r * stride + c] = clip(top[c], max);
                }
            }
        }
        1 => {
            for r in 0..16 {
                plane[base + r * stride..base + r * stride + 16].fill(clip(left[r], max));
            }
        }
        2 => {
            let st: i32 = top.iter().sum();
            let sl: i32 = left.iter().sum();
            let dc = match (a.top, a.left) {
                (true, true) => (st + sl + 16) >> 5,
                (false, true) => (sl + 8) >> 4,
                (true, false) => (st + 8) >> 4,
                _ => mid,
            };
            for r in 0..16 {
                plane[base + r * stride..base + r * stride + 16].fill(clip(dc, max));
            }
        }
        _ => {
            let tl = if a.top_left { i32::from(plane[(y - 1) * stride + x - 1]) } else { mid };
            let t = |i: i32| if i < 0 { tl } else { top.get(i as usize).copied().unwrap_or(tl) };
            let l = |i: i32| if i < 0 { tl } else { left.get(i as usize).copied().unwrap_or(tl) };
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
                for cc in 0..16 {
                    plane[base + r * stride + cc] = clip((aa + b * (cc as i32 - 7) + c * (r as i32 - 7) + 16) >> 5, max);
                }
            }
        }
    }
}

/// Chroma intra prediction for one component's macroblock block (8.3.4): 8x8 at chroma (x, y) for
/// 4:2:0 (`chroma422 == false`), 8x16 for 4:2:2 (`chroma422 == true`, ChromaArrayType == 2).
/// Modes (Table 8-5): 0 DC, 1 horizontal, 2 vertical, 3 plane.
#[allow(clippy::too_many_arguments)]
pub fn pred_chroma(plane: &mut [u16], stride: usize, x: usize, y: usize, mode: u8, a: Avail, max: i32, chroma422: bool) {
    const W: usize = 8;
    let h = if chroma422 { 16 } else { 8 };
    if !fits(plane, stride, x, y, W, h, W, a) {
        return;
    }
    let mid = mid(max);
    // As in pred16x16: unavailable edges stay 0 (spec-forbidden combinations only).
    let mut top = [0i32; W];
    let mut left = [0i32; 16];
    if a.top {
        for (i, t) in top.iter_mut().enumerate() {
            *t = i32::from(plane[(y - 1) * stride + x + i]);
        }
    }
    if a.left {
        for (i, l) in left.iter_mut().enumerate().take(h) {
            *l = i32::from(plane[(y + i) * stride + x - 1]);
        }
    }
    let base = y * stride + x;
    match mode {
        0 => {
            // DC per 4x4 sub-block (8.3.4.1): chroma4x4BlkIdx 0..3 in 4:2:0, 0..7 in 4:2:2. Each
            // block averages its own 4 top and 4 left reference samples (8-132..8-141).
            for by in 0..h / 4 {
                for bx in 0..W / 4 {
                    let st: i32 = top[bx * 4..bx * 4 + 4].iter().sum();
                    let sl: i32 = left[by * 4..by * 4 + 4].iter().sum();
                    let dc = if (bx == 0 && by == 0) || (bx > 0 && by > 0) {
                        // (xO, yO) = (0, 0) or xO > 0 and yO > 0 (8-132..8-135).
                        match (a.top, a.left) {
                            (true, true) => (st + sl + 4) >> 3,
                            (false, true) => (sl + 2) >> 2,
                            (true, false) => (st + 2) >> 2,
                            _ => mid,
                        }
                    } else if bx > 0 {
                        // xO > 0, yO == 0 (8-136..8-138): top first, left fallback.
                        if a.top {
                            (st + 2) >> 2
                        } else if a.left {
                            (sl + 2) >> 2
                        } else {
                            mid
                        }
                    } else if a.left {
                        // xO == 0, yO > 0 (8-139..8-141): left first, top fallback.
                        (sl + 2) >> 2
                    } else if a.top {
                        (st + 2) >> 2
                    } else {
                        mid
                    };
                    for r in 0..4 {
                        let o = base + (by * 4 + r) * stride + bx * 4;
                        plane[o..o + 4].fill(clip(dc, max));
                    }
                }
            }
        }
        1 => {
            // Horizontal (8-142): predC[x, y] = p[-1, y].
            for r in 0..h {
                plane[base + r * stride..base + r * stride + W].fill(clip(left[r], max));
            }
        }
        2 => {
            // Vertical (8-143): predC[x, y] = p[x, -1].
            for r in 0..h {
                for c in 0..W {
                    plane[base + r * stride + c] = clip(top[c], max);
                }
            }
        }
        _ => {
            // Plane (8.3.4.4, 8-144..8-149): xCF = 0 and yCF = 4 for 4:2:2 (0 for 4:2:0).
            let ycf = if chroma422 { 4 } else { 0 };
            let tl = if a.top_left { i32::from(plane[(y - 1) * stride + x - 1]) } else { mid };
            let t = |i: i32| if i < 0 { tl } else { top.get(i as usize).copied().unwrap_or(tl) };
            let l = |i: i32| if i < 0 { tl } else { left.get(i as usize).copied().unwrap_or(tl) };
            // H (8-148) over x' = 0..3; V (8-149) over y' = 0..3+yCF (0..7 for 4:2:2).
            let mut hh = 0;
            for k in 0..4 {
                hh += (k + 1) * (t(4 + k) - t(2 - k));
            }
            let mut vv = 0;
            for k in 0..4 + ycf {
                vv += (k + 1) * (l(4 + ycf + k) - l(2 + ycf - k));
            }
            // a (8-145); b (8-146) = 34 for ChromaArrayType != 3; c (8-147) = 5 for
            // ChromaArrayType == 2 and 34 for ChromaArrayType == 1.
            let aa = 16 * (l(h as i32 - 1) + t(W as i32 - 1));
            let b = (34 * hh + 32) >> 6;
            let c = ((if chroma422 { 5 } else { 34 }) * vv + 32) >> 6;
            for r in 0..h {
                for cc in 0..W {
                    plane[base + r * stride + cc] = clip((aa + b * (cc as i32 - 3) + c * (r as i32 - 3 - ycf) + 16) >> 5, max);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Avail = Avail { left: true, top: true, top_left: true, top_right: true };
    const MAX8: i32 = 255;
    const MAX10: i32 = 1023;

    fn setup() -> (Vec<u16>, usize) {
        // 16x16 plane; block at (4,4); top row y=3 = 10,20,..; left column x=3 = 1,2,3,...
        let stride = 16;
        let mut p = vec![0u16; stride * 16];
        for i in 0..12 {
            p[3 * stride + 4 + i] = (10 * (i + 1)) as u16;
        }
        for i in 0..8 {
            p[(4 + i) * stride + 3] = (i + 1) as u16;
        }
        p[3 * stride + 3] = 5;
        (p, stride)
    }

    fn setup422() -> (Vec<u16>, usize) {
        // 24x24 plane; 8x16 chroma block at (4,4); top row y=3 = 10,20,..80; left column x=3 =
        // 1..16; tl = 5.
        let stride = 24;
        let mut p = vec![0u16; stride * 24];
        for i in 0..8 {
            p[3 * stride + 4 + i] = (10 * (i + 1)) as u16;
        }
        for i in 0..16 {
            p[(4 + i) * stride + 3] = (i + 1) as u16;
        }
        p[3 * stride + 3] = 5;
        (p, stride)
    }

    #[test]
    fn i4_vertical_horizontal_dc() {
        let (mut p, s) = setup();
        pred4x4(&mut p, s, 4, 4, 0, ALL, MAX8);
        assert_eq!(&p[4 * s + 4..4 * s + 8], &[10, 20, 30, 40]);
        assert_eq!(&p[7 * s + 4..7 * s + 8], &[10, 20, 30, 40]);
        pred4x4(&mut p, s, 4, 4, 1, ALL, MAX8);
        assert_eq!(&p[5 * s + 4..5 * s + 8], &[2, 2, 2, 2]);
        pred4x4(&mut p, s, 4, 4, 2, ALL, MAX8);
        // (10+20+30+40 + 1+2+3+4 + 4) >> 3 = 114 >> 3 = 14
        assert_eq!(p[4 * s + 4], 14);
    }

    #[test]
    fn i4_diag_down_left_hand_computed() {
        let (mut p, s) = setup();
        pred4x4(&mut p, s, 4, 4, 3, ALL, MAX8);
        // pred[0,0] = (t0 + 2 t1 + t2 + 2) >> 2 = (10 + 40 + 30 + 2) >> 2 = 20
        assert_eq!(p[4 * s + 4], 20);
        // pred[3,3] = (t6 + 3 t7 + 2) >> 2 = (70 + 240 + 2) >> 2 = 78
        assert_eq!(p[7 * s + 7], 78);
    }

    #[test]
    fn i4_diag_down_right_hand_computed() {
        let (mut p, s) = setup();
        pred4x4(&mut p, s, 4, 4, 4, ALL, MAX8);
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
        pred4x4(&mut p, s, 4, 4, 3, a, MAX8);
        // top-right replaced by t3 = 40: pred[3,3] = (40 + 120 + 2) >> 2 = 40
        assert_eq!(p[7 * s + 7], 40);
    }

    #[test]
    fn i16_plane_flat_is_flat() {
        let s = 32;
        let mut p = vec![77u16; s * 32];
        pred16x16(&mut p, s, 8, 8, 3, ALL, MAX8);
        assert!(p[8 * s + 8..8 * s + 24].iter().all(|&v| v == 77));
    }

    #[test]
    fn i4_dc_with_no_neighbours_is_mid_grey() {
        // 8-51: pred4x4L[x, y] = (1 << (BitDepthY - 1)) when nothing is available.
        let s = 16;
        let none = Avail::default();
        let mut p = vec![0u16; s * 16];
        pred4x4(&mut p, s, 4, 4, 2, none, MAX8);
        assert_eq!(p[4 * s + 4], 128);
        let mut q = vec![0u16; s * 16];
        pred4x4(&mut q, s, 4, 4, 2, none, MAX10);
        assert_eq!(q[4 * s + 4], 512);
    }

    #[test]
    fn chroma_dc_rules() {
        let s = 16;
        let mut p = vec![0u16; s * 16];
        for i in 0..8 {
            p[3 * s + 4 + i] = if i < 4 { 40 } else { 80 };
            p[(4 + i) * s + 3] = if i < 4 { 8 } else { 16 };
        }
        pred_chroma(&mut p, s, 4, 4, 0, ALL, MAX8, false);
        assert_eq!(p[4 * s + 4], (160 + 32 + 4) >> 3); // top-left block: both
        assert_eq!(p[4 * s + 8], 80); // top-right block: top only
        assert_eq!(p[8 * s + 4], 16); // bottom-left: left only
        assert_eq!(p[8 * s + 8], ((320 + 64 + 4) >> 3) as u16); // bottom-right: both
    }

    #[test]
    fn chroma422_dc_averages_per_4x4_subblock() {
        // 8-132..8-141 over the 2x4 grid of 4x4 sub-blocks (chroma4x4BlkIdx 0..7):
        // top = 10,20,..80 (y=3), left = 1..16 (x=3).
        let (mut p, s) = setup422();
        pred_chroma(&mut p, s, 4, 4, 0, ALL, MAX8, true);
        let row = |r: usize| &p[(4 + r) * s + 4..(4 + r) * s + 12];
        // (0,0): both -> (10+20+30+40 + 1+2+3+4 + 4) >> 3 = 14; (4,0): top only -> (50+60+70+80+2)>>2 = 65
        assert_eq!(row(0), &[14, 14, 14, 14, 65, 65, 65, 65]);
        // (0,4): left only -> (5+6+7+8+2)>>2 = 7; (4,4): both -> (260 + 26 + 4) >> 3 = 36
        assert_eq!(row(4), &[7, 7, 7, 7, 36, 36, 36, 36]);
        // (0,8): left only -> (9+10+11+12+2)>>2 = 11; (4,8): both -> (260 + 42 + 4) >> 3 = 38
        assert_eq!(row(8), &[11, 11, 11, 11, 38, 38, 38, 38]);
        // (0,12): left only -> (13+14+15+16+2)>>2 = 15; (4,12): both -> (260 + 58 + 4) >> 3 = 40
        assert_eq!(row(12), &[15, 15, 15, 15, 40, 40, 40, 40]);
        // 16 rows are predicted.
        assert_eq!(row(15), row(12));
    }

    #[test]
    fn chroma422_dc_availability_fallbacks() {
        let (mut p, s) = setup422();
        let left_only = Avail { left: true, top: false, top_left: false, top_right: false };
        pred_chroma(&mut p, s, 4, 4, 0, left_only, MAX8, true);
        let row = |r: usize| &p[(4 + r) * s + 4..(4 + r) * s + 12];
        // Top missing: case A falls back to the left average (8-133) and the xO > 0, yO == 0 block
        // falls back to its left average (8-137): (1+2+3+4+2)>>2 = 3, then 7, 11, 15 down the rows.
        assert_eq!(row(0), &[3, 3, 3, 3, 3, 3, 3, 3]);
        assert_eq!(row(4), &[7, 7, 7, 7, 7, 7, 7, 7]);
        assert_eq!(row(12), &[15, 15, 15, 15, 15, 15, 15, 15]);

        // Nothing available: mid grey (8-135/8-138/8-141).
        let block_is = |p: &[u16], s: usize, v: u16| (0..16).all(|r| p[(4 + r) * s + 4..(4 + r) * s + 12].iter().all(|&x| x == v));
        let (mut q, s) = setup422();
        pred_chroma(&mut q, s, 4, 4, 0, Avail::default(), MAX8, true);
        assert!(block_is(&q, s, 128));
        let (mut r, s) = setup422();
        pred_chroma(&mut r, s, 4, 4, 0, Avail::default(), MAX10, true);
        assert!(block_is(&r, s, 512));
    }

    #[test]
    fn chroma422_horizontal_vertical_fill_16_lines() {
        let (mut p, s) = setup422();
        pred_chroma(&mut p, s, 4, 4, 2, ALL, MAX8, true); // vertical: p[x, -1]
        for r in 0..16 {
            assert_eq!(&p[(4 + r) * s + 4..(4 + r) * s + 12], &[10, 20, 30, 40, 50, 60, 70, 80], "vertical row {r}");
        }
        let (mut q, s) = setup422();
        pred_chroma(&mut q, s, 4, 4, 1, ALL, MAX8, true); // horizontal: p[-1, y]
        for r in 0..16 {
            let v = (r + 1) as u16;
            assert_eq!(&q[(4 + r) * s + 4..(4 + r) * s + 12], &[v; 8], "horizontal row {r}");
        }
    }

    #[test]
    fn chroma422_plane_hand_computed() {
        // 8-144..8-149 with top = 10..80, left = 1..16, tl = 5:
        // a = 16*(16 + 80) = 1536; H = 580; V = 368; b = (34*580+32)>>6 = 308; c = (5*368+32)>>6 = 29.
        let (mut p, s) = setup422();
        pred_chroma(&mut p, s, 4, 4, 3, ALL, MAX8, true);
        assert_eq!(p[4 * s + 4], 13); // (1536 - 3*308 - 7*29 + 16) >> 5 = 425 >> 5
        assert_eq!(p[11 * s + 7], 48); // (1536 + 16) >> 5 = 48 at (x=3, y=7)
        assert_eq!(p[19 * s + 11], 94); // (1536 + 4*308 + 8*29 + 16) >> 5 = 3016 >> 5
    }

    #[test]
    fn chroma422_plane_flat_is_flat_at_10_bit() {
        let s = 24;
        let mut p = vec![512u16; s * 24];
        pred_chroma(&mut p, s, 4, 4, 3, ALL, MAX10, true);
        assert!((0..16).all(|r| p[(4 + r) * s + 4..(4 + r) * s + 12].iter().all(|&v| v == 512)));
    }

    #[test]
    fn malformed_geometry_predicts_nothing() {
        // A block (or its neighbours) outside the plane must not panic and must not write.
        let s = 8;
        let mut p = vec![9u16; s * 8];
        let before = p.clone();
        pred4x4(&mut p, s, 6, 6, 0, ALL, MAX8); // block runs past the plane
        pred8x8(&mut p, s, 0, 0, 3, ALL, MAX8); // neighbours at x = -1 / y = -1
        pred16x16(&mut p, s, 4, 4, 2, ALL, MAX8); // block larger than the plane
        pred_chroma(&mut p, s, 4, 4, 0, ALL, MAX8, true); // 8x16 does not fit
        pred_chroma(&mut p, s, 200, 200, 1, ALL, MAX8, false); // far out of range
        assert_eq!(p, before);
    }
}
