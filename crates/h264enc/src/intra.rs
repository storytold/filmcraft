//! Intra prediction (§8.3): 4x4, 8x8 (with reference filtering), 16x16 and chroma.

use crate::pixel::{Depth, Pixel};

/// Neighbouring samples of a block. `top` holds p[x,-1] for x = 0..2N (top then top-right), `left` p[-1,y].
#[derive(Clone, Copy)]
pub struct Edge<P, const N2: usize, const N: usize> {
    pub top: [P; N2],
    pub left: [P; N],
    pub tl: P,
    pub has_top: bool,
    pub has_left: bool,
    pub has_tl: bool,
}

pub type Edge4<P> = Edge<P, 8, 4>;
pub type Edge8<P> = Edge<P, 16, 8>;

/// Which modes are usable given neighbour availability.
pub fn mode_available(mode: u8, has_top: bool, has_left: bool, has_tl: bool) -> bool {
    match mode {
        0 | 3 | 7 => has_top,
        1 | 8 => has_left,
        2 => true,
        _ => has_top && has_left && has_tl,
    }
}

/// Generic directional prediction for an NxN block given (already filtered, for 8x8) neighbours.
/// `t(x)` = p[x,-1] for x in -1..2N, `l(y)` = p[-1,y] for y in -1..N.
fn pred_dir<P: Pixel, const N: usize>(mode: u8, t: &dyn Fn(i32) -> i32, l: &dyn Fn(i32) -> i32, dc: i32, out: &mut [P]) {
    let n = N as i32;
    for y in 0..n {
        for x in 0..n {
            let v = match mode {
                0 => t(x),
                1 => l(y),
                2 => dc,
                3 => {
                    if x == n - 1 && y == n - 1 {
                        (t(2 * n - 2) + 3 * t(2 * n - 1) + 2) >> 2
                    } else {
                        (t(x + y) + 2 * t(x + y + 1) + t(x + y + 2) + 2) >> 2
                    }
                }
                4 => {
                    if x > y {
                        (t(x - y - 2) + 2 * t(x - y - 1) + t(x - y) + 2) >> 2
                    } else if x < y {
                        (l(y - x - 2) + 2 * l(y - x - 1) + l(y - x) + 2) >> 2
                    } else {
                        (t(0) + 2 * t(-1) + l(0) + 2) >> 2
                    }
                }
                5 => {
                    let z = 2 * x - y;
                    if z >= 0 && z & 1 == 0 {
                        (t(x - (y >> 1) - 1) + t(x - (y >> 1)) + 1) >> 1
                    } else if z >= 0 {
                        (t(x - (y >> 1) - 2) + 2 * t(x - (y >> 1) - 1) + t(x - (y >> 1)) + 2) >> 2
                    } else if z == -1 {
                        (l(0) + 2 * l(-1) + t(0) + 2) >> 2
                    } else {
                        (l(y - 2 * x - 1) + 2 * l(y - 2 * x - 2) + l(y - 2 * x - 3) + 2) >> 2
                    }
                }
                6 => {
                    let z = 2 * y - x;
                    if z >= 0 && z & 1 == 0 {
                        (l(y - (x >> 1) - 1) + l(y - (x >> 1)) + 1) >> 1
                    } else if z >= 0 {
                        (l(y - (x >> 1) - 2) + 2 * l(y - (x >> 1) - 1) + l(y - (x >> 1)) + 2) >> 2
                    } else if z == -1 {
                        (l(0) + 2 * l(-1) + t(0) + 2) >> 2
                    } else {
                        (t(x - 2 * y - 1) + 2 * t(x - 2 * y - 2) + t(x - 2 * y - 3) + 2) >> 2
                    }
                }
                7 => {
                    if y & 1 == 0 {
                        (t(x + (y >> 1)) + t(x + (y >> 1) + 1) + 1) >> 1
                    } else {
                        (t(x + (y >> 1)) + 2 * t(x + (y >> 1) + 1) + t(x + (y >> 1) + 2) + 2) >> 2
                    }
                }
                _ => {
                    let z = x + 2 * y;
                    let lim = 2 * n - 3;
                    if z < lim && z & 1 == 0 {
                        (l(y + (x >> 1)) + l(y + (x >> 1) + 1) + 1) >> 1
                    } else if z < lim {
                        (l(y + (x >> 1)) + 2 * l(y + (x >> 1) + 1) + l(y + (x >> 1) + 2) + 2) >> 2
                    } else if z == lim {
                        (l(n - 2) + 3 * l(n - 1) + 2) >> 2
                    } else {
                        l(n - 1)
                    }
                }
            };
            out[(y * n + x) as usize] = P::of(v);
        }
    }
}

/// Intra 4x4 prediction. `e.top[4..8]` must already be substituted with top[3] when top-right is unavailable.
pub fn pred4x4<P: Pixel>(mode: u8, e: &Edge4<P>, d: Depth, out: &mut [P; 16]) {
    let dc = match (e.has_top, e.has_left) {
        (true, true) => (e.top[..4].iter().map(|&v| v.get()).sum::<i32>() + e.left.iter().map(|&v| v.get()).sum::<i32>() + 4) >> 3,
        (true, false) => (e.top[..4].iter().map(|&v| v.get()).sum::<i32>() + 2) >> 2,
        (false, true) => (e.left.iter().map(|&v| v.get()).sum::<i32>() + 2) >> 2,
        _ => d.mid,
    };
    if mode == 2 {
        out.fill(P::of(dc));
        return;
    }
    let t = |x: i32| if x < 0 { e.tl.get() } else { e.top[x as usize].get() };
    let l = |y: i32| if y < 0 { e.tl.get() } else { e.left[y as usize].get() };
    pred_dir::<P, 4>(mode, &t, &l, dc, out);
}

/// Reference sample filtering for Intra 8x8 (§8.3.2.2.1). `e.top[8..16]` must be substituted if unavailable.
pub fn filter8<P: Pixel>(e: &Edge8<P>) -> Edge8<P> {
    let mut f = *e;
    if e.has_top {
        let p = |x: usize| e.top[x].get();
        f.top[0] = P::of(if e.has_tl { (e.tl.get() + 2 * p(0) + p(1) + 2) >> 2 } else { (3 * p(0) + p(1) + 2) >> 2 });
        for x in 1..15 {
            f.top[x] = P::of((p(x - 1) + 2 * p(x) + p(x + 1) + 2) >> 2);
        }
        f.top[15] = P::of((p(14) + 3 * p(15) + 2) >> 2);
    }
    if e.has_tl {
        let tl = e.tl.get();
        f.tl = if !e.has_top || !e.has_left {
            if e.has_top {
                P::of((3 * tl + e.top[0].get() + 2) >> 2)
            } else if e.has_left {
                P::of((3 * tl + e.left[0].get() + 2) >> 2)
            } else {
                e.tl
            }
        } else {
            P::of((e.top[0].get() + 2 * tl + e.left[0].get() + 2) >> 2)
        };
    }
    if e.has_left {
        let p = |y: usize| e.left[y].get();
        f.left[0] = P::of(if e.has_tl { (e.tl.get() + 2 * p(0) + p(1) + 2) >> 2 } else { (3 * p(0) + p(1) + 2) >> 2 });
        for y in 1..7 {
            f.left[y] = P::of((p(y - 1) + 2 * p(y) + p(y + 1) + 2) >> 2);
        }
        f.left[7] = P::of((p(6) + 3 * p(7) + 2) >> 2);
    }
    f
}

/// Intra 8x8 prediction from *filtered* neighbours.
pub fn pred8x8<P: Pixel>(mode: u8, f: &Edge8<P>, d: Depth, out: &mut [P; 64]) {
    let dc = match (f.has_top, f.has_left) {
        (true, true) => (f.top[..8].iter().map(|&v| v.get()).sum::<i32>() + f.left.iter().map(|&v| v.get()).sum::<i32>() + 8) >> 4,
        (true, false) => (f.top[..8].iter().map(|&v| v.get()).sum::<i32>() + 4) >> 3,
        (false, true) => (f.left.iter().map(|&v| v.get()).sum::<i32>() + 4) >> 3,
        _ => d.mid,
    };
    if mode == 2 {
        out.fill(P::of(dc));
        return;
    }
    let t = |x: i32| if x < 0 { f.tl.get() } else { f.top[x as usize].get() };
    let l = |y: i32| if y < 0 { f.tl.get() } else { f.left[y as usize].get() };
    pred_dir::<P, 8>(mode, &t, &l, dc, out);
}

/// Intra 16x16 prediction. Modes: 0 V, 1 H, 2 DC, 3 plane.
#[allow(clippy::too_many_arguments)]
pub fn pred16x16<P: Pixel>(mode: u8, top: &[P; 16], left: &[P; 16], tl: P, has_top: bool, has_left: bool, d: Depth, out: &mut [P; 256]) {
    match mode {
        0 => {
            for y in 0..16 {
                out[y * 16..y * 16 + 16].copy_from_slice(top);
            }
        }
        1 => {
            for y in 0..16 {
                out[y * 16..y * 16 + 16].fill(left[y]);
            }
        }
        2 => {
            let st: i32 = top.iter().map(|&v| v.get()).sum();
            let sl: i32 = left.iter().map(|&v| v.get()).sum();
            let dc = match (has_top, has_left) {
                (true, true) => (st + sl + 16) >> 5,
                (true, false) => (st + 8) >> 4,
                (false, true) => (sl + 8) >> 4,
                _ => d.mid,
            };
            out.fill(P::of(dc));
        }
        _ => {
            let p_t = |x: i32| if x < 0 { tl.get() } else { top[x as usize].get() };
            let p_l = |y: i32| if y < 0 { tl.get() } else { left[y as usize].get() };
            let mut h = 0;
            let mut v = 0;
            for i in 0..8 {
                h += (i + 1) * (p_t(8 + i) - p_t(6 - i));
                v += (i + 1) * (p_l(8 + i) - p_l(6 - i));
            }
            let a = 16 * (p_l(15) + p_t(15));
            let b = (5 * h + 32) >> 6;
            let c = (5 * v + 32) >> 6;
            for y in 0..16 {
                for x in 0..16 {
                    out[y * 16 + x] = P::clip((a + b * (x as i32 - 7) + c * (y as i32 - 7) + 16) >> 5, d);
                }
            }
        }
    }
}

/// Chroma 8x8 prediction (4:2:0). Modes: 0 DC, 1 H, 2 V, 3 plane.
#[allow(clippy::too_many_arguments)]
pub fn pred_chroma<P: Pixel>(mode: u8, top: &[P; 8], left: &[P; 8], tl: P, has_top: bool, has_left: bool, d: Depth, out: &mut [P; 64]) {
    match mode {
        0 => {
            for by in 0..2 {
                for bx in 0..2 {
                    let st: i32 = top[bx * 4..bx * 4 + 4].iter().map(|&v| v.get()).sum();
                    let sl: i32 = left[by * 4..by * 4 + 4].iter().map(|&v| v.get()).sum();
                    let dc = if (bx == 0 && by == 0) || (bx == 1 && by == 1) {
                        match (has_top, has_left) {
                            (true, true) => (st + sl + 4) >> 3,
                            (true, false) => (st + 2) >> 2,
                            (false, true) => (sl + 2) >> 2,
                            _ => d.mid,
                        }
                    } else if bx == 1 && by == 0 {
                        if has_top {
                            (st + 2) >> 2
                        } else if has_left {
                            (sl + 2) >> 2
                        } else {
                            d.mid
                        }
                    } else if has_left {
                        (sl + 2) >> 2
                    } else if has_top {
                        (st + 2) >> 2
                    } else {
                        d.mid
                    };
                    for y in 0..4 {
                        for x in 0..4 {
                            out[(by * 4 + y) * 8 + bx * 4 + x] = P::of(dc);
                        }
                    }
                }
            }
        }
        1 => {
            for y in 0..8 {
                out[y * 8..y * 8 + 8].fill(left[y]);
            }
        }
        2 => {
            for y in 0..8 {
                out[y * 8..y * 8 + 8].copy_from_slice(top);
            }
        }
        _ => {
            let p_t = |x: i32| if x < 0 { tl.get() } else { top[x as usize].get() };
            let p_l = |y: i32| if y < 0 { tl.get() } else { left[y as usize].get() };
            let mut h = 0;
            let mut v = 0;
            for i in 0..4 {
                h += (i + 1) * (p_t(4 + i) - p_t(2 - i));
                v += (i + 1) * (p_l(4 + i) - p_l(2 - i));
            }
            let a = 16 * (p_l(7) + p_t(7));
            let b = (34 * h + 32) >> 6;
            let c = (34 * v + 32) >> 6;
            for y in 0..8 {
                for x in 0..8 {
                    out[y * 8 + x] = P::clip((a + b * (x as i32 - 3) + c * (y as i32 - 3) + 16) >> 5, d);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_and_vertical() {
        let e: Edge4<u8> = Edge4 { top: [10, 20, 30, 40, 40, 40, 40, 40], left: [1, 2, 3, 4], tl: 5, has_top: true, has_left: true, has_tl: true };
        let d = Depth::new(8);
        let mut o = [0u8; 16];
        pred4x4(0, &e, d, &mut o);
        assert_eq!(&o[12..16], &[10, 20, 30, 40]);
        pred4x4(2, &e, d, &mut o);
        assert_eq!(o[0], ((100 + 10 + 4) >> 3) as u8);
        // horizontal up saturates at bottom-left
        pred4x4(8, &e, d, &mut o);
        assert_eq!(o[15], 4);
    }
}
