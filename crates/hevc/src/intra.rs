//! Intra sample prediction (8.4.4.2): reference substitution, filtering (incl. strong intra
//! smoothing), planar, DC and the 33 angular modes.

use crate::picture::Sample;
use crate::spec_tables::{INTRA_PRED_ANGLE, INV_ANGLE};

/// Reference samples of an nTbS block: `left[0]` = `top[0]` = p[-1][-1], `left[1 + y]` = p[-1][y],
/// `top[1 + x]` = p[x][-1] for x, y = 0..2*nTbS-1.
#[derive(Clone)]
pub struct Refs {
    pub left: [u16; 129],
    pub top: [u16; 129],
}

impl Default for Refs {
    fn default() -> Self {
        Refs { left: [0; 129], top: [0; 129] }
    }
}

/// Substitution process (8.4.4.2.2). `avail_left[1 + y]` / `avail_top[1 + x]` (index 0 = corner) tell
/// which samples are available; unavailable ones are replaced.
pub fn substitute(r: &mut Refs, avail_left: &[bool; 129], avail_top: &[bool; 129], n: usize, bit_depth: u32) {
    let n2 = 2 * n;
    // Sequence order: p[-1][2n-1] .. p[-1][-1], p[0][-1] .. p[2n-1][-1]
    let any = avail_left[..=n2].iter().any(|&a| a) || avail_top[1..=n2].iter().any(|&a| a);
    if !any {
        let v = 1u16 << (bit_depth - 1);
        r.left[..=n2].fill(v);
        r.top[..=n2].fill(v);
        return;
    }
    if !avail_left[n2] {
        // search upwards then along the top
        let mut found = None;
        for y in (0..n2).rev() {
            if avail_left[y] {
                found = Some(r.left[y]);
                break;
            }
        }
        if found.is_none() {
            for x in 1..=n2 {
                if avail_top[x] {
                    found = Some(r.top[x]);
                    break;
                }
            }
        }
        r.left[n2] = found.unwrap_or(0);
    }
    for y in (0..n2).rev() {
        if !avail_left[y] {
            r.left[y] = r.left[y + 1];
        }
    }
    r.top[0] = r.left[0];
    for x in 1..=n2 {
        if !avail_top[x] {
            r.top[x] = r.top[x - 1];
        }
    }
}

/// Filtering process of neighbouring samples (8.4.4.2.3) for luma (and 4:4:4 chroma).
pub fn filter(r: &mut Refs, mode: u32, n: usize, strong_enabled: bool, is_luma: bool, bit_depth: u32) {
    if mode == 1 || n == 4 {
        return;
    }
    let min_dist = (mode as i32 - 26).abs().min((mode as i32 - 10).abs());
    let thres = match n {
        8 => 7,
        16 => 1,
        _ => 0,
    };
    if min_dist <= thres {
        return;
    }
    let n2 = 2 * n;
    let c = r.left[0] as i32;
    if strong_enabled && is_luma && n == 32 {
        let (bl, tr) = (r.left[n2] as i32, r.top[n2] as i32);
        let thr = 1 << (bit_depth - 5);
        if (c + tr - 2 * r.top[n] as i32).abs() < thr && (c + bl - 2 * r.left[n] as i32).abs() < thr {
            for i in 0..63 {
                r.left[1 + i] = (((63 - i as i32) * c + (i as i32 + 1) * bl + 32) >> 6) as u16;
                r.top[1 + i] = (((63 - i as i32) * c + (i as i32 + 1) * tr + 32) >> 6) as u16;
            }
            return;
        }
    }
    let (l, t) = (r.left, r.top);
    let corner = ((l[1] as u32 + 2 * c as u32 + t[1] as u32 + 2) >> 2) as u16;
    for i in 1..n2 {
        r.left[i] = ((l[i - 1] as u32 + 2 * l[i] as u32 + l[i + 1] as u32 + 2) >> 2) as u16;
        r.top[i] = ((t[i - 1] as u32 + 2 * t[i] as u32 + t[i + 1] as u32 + 2) >> 2) as u16;
    }
    r.left[0] = corner;
    r.top[0] = corner;
}

/// Predict an n x n block with `mode` into `out` (stride `os`). `edge_filters`: DC / horizontal /
/// vertical boundary smoothing (luma blocks smaller than 32x32).
pub fn predict<S: Sample>(r: &Refs, mode: u32, n: usize, edge_filters: bool, bit_depth: u32, out: &mut [S], os: usize) {
    match n {
        4 => predict_n::<S, 4>(r, mode, edge_filters, bit_depth, out, os),
        8 => predict_n::<S, 8>(r, mode, edge_filters, bit_depth, out, os),
        16 => predict_n::<S, 16>(r, mode, edge_filters, bit_depth, out, os),
        _ => predict_n::<S, 32>(r, mode, edge_filters, bit_depth, out, os),
    }
}

#[inline(always)]
fn predict_n<S: Sample, const N: usize>(r: &Refs, mode: u32, edge_filters: bool, bit_depth: u32, out: &mut [S], os: usize) {
    match mode {
        0 => planar::<S, N>(r, out, os),
        1 => {
            let log2 = N.trailing_zeros();
            let sum: u32 = r.top[1..=N].iter().map(|&v| v as u32).sum::<u32>() + r.left[1..=N].iter().map(|&v| v as u32).sum::<u32>();
            let dc = ((sum + N as u32) >> (log2 + 1)) as u16;
            let dcs = S::from_u16(dc);
            for y in 0..N {
                out[y * os..y * os + N].fill(dcs);
            }
            if edge_filters && N < 32 {
                let d = dc as u32;
                out[0] = S::from_i32(((r.left[1] as u32 + 2 * d + r.top[1] as u32 + 2) >> 2) as i32);
                for x in 1..N {
                    out[x] = S::from_i32(((r.top[1 + x] as u32 + 3 * d + 2) >> 2) as i32);
                }
                for y in 1..N {
                    out[y * os] = S::from_i32(((r.left[1 + y] as u32 + 3 * d + 2) >> 2) as i32);
                }
            }
        }
        _ => angular::<S, N>(r, mode, edge_filters, bit_depth, out, os),
    }
}

fn planar<S: Sample, const N: usize>(r: &Refs, out: &mut [S], os: usize) {
    let log2 = N.trailing_zeros();
    let tr = r.top[1 + N] as i32;
    let bl = r.left[1 + N] as i32;
    let top = &r.top[1..1 + N];
    for y in 0..N {
        let l = r.left[1 + y] as i32;
        let (wt, wb) = ((N - 1 - y) as i32, (y + 1) as i32);
        for (x, (o, &t)) in out[y * os..y * os + N].iter_mut().zip(top).enumerate() {
            let v = (N - 1 - x) as i32 * l + (x as i32 + 1) * tr + wt * t as i32 + wb * bl + N as i32;
            *o = S::from_i32(v >> (log2 + 1));
        }
    }
}

/// Angular modes 2..34 (8.4.4.2.6). Horizontal modes (< 18) are predicted as the vertical mode
/// on the transposed references into a tile, then transposed into `out`.
/// Row `j` of a vertical angular prediction from `ref[x] = refs[base + x]`.
#[inline(always)]
fn angular_row<const N: usize>(refs: &[u16], base: i32, angle: i32, j: usize) -> [u16; N] {
    let pos = (j as i32 + 1) * angle;
    let fact = (pos & 31) as u32;
    let p = &refs[(base + (pos >> 5) + 1) as usize..][..N + 1];
    let mut v = [0u16; N];
    if fact == 0 {
        v.copy_from_slice(&p[..N]);
    } else {
        for (v, (&a, &b)) in v.iter_mut().zip(p.iter().zip(&p[1..])) {
            *v = (((32 - fact) * a as u32 + fact * b as u32 + 16) >> 5) as u16;
        }
    }
    v
}

fn angular<S: Sample, const N: usize>(r: &Refs, mode: u32, edge_filters: bool, bit_depth: u32, out: &mut [S], os: usize) {
    let angle = INTRA_PRED_ANGLE[mode as usize] as i32;
    let (main, side) = if mode >= 18 { (&r.top, &r.left) } else { (&r.left, &r.top) };
    // ref[x] = refs[base + x] for x = -N..=2N; ref[x] = main[x] for x >= 0
    let mut ext = [0u16; 3 * 32 + 1];
    let (refs, base): (&[u16], i32) = if angle < 0 {
        ext[N..=2 * N].copy_from_slice(&main[..=N]);
        let lim = (N as i32 * angle) >> 5;
        if lim < -1 {
            let inv = INV_ANGLE[(mode - 11) as usize] as i32;
            for x in lim..=-1 {
                ext[(N as i32 + x) as usize] = side[((x * inv + 128) >> 8) as usize];
            }
        }
        (&ext, N as i32)
    } else {
        (main, 0)
    };
    if mode >= 18 {
        for j in 0..N {
            for (o, v) in out[j * os..j * os + N].iter_mut().zip(angular_row::<N>(refs, base, angle, j)) {
                *o = S::from_u16(v);
            }
        }
    } else {
        let mut tile = [[0u16; N]; N];
        for (j, t) in tile.iter_mut().enumerate() {
            *t = angular_row::<N>(refs, base, angle, j);
        }
        for y in 0..N {
            for (o, t) in out[y * os..y * os + N].iter_mut().zip(&tile) {
                *o = S::from_u16(t[y]);
            }
        }
    }
    if edge_filters && N < 32 {
        let max = (1i32 << bit_depth) - 1;
        if mode == 26 {
            for y in 0..N {
                let v = r.top[1] as i32 + ((r.left[1 + y] as i32 - r.left[0] as i32) >> 1);
                out[y * os] = S::from_i32(v.clamp(0, max));
            }
        } else if mode == 10 {
            for x in 0..N {
                let v = r.left[1] as i32 + ((r.top[1 + x] as i32 - r.top[0] as i32) >> 1);
                out[x] = S::from_i32(v.clamp(0, max));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(f: impl Fn(i32) -> u16) -> Refs {
        let mut r = Refs::default();
        r.left[0] = f(0);
        r.top[0] = f(0);
        for i in 1..129 {
            r.left[i] = f(-(i as i32));
            r.top[i] = f(i as i32);
        }
        r
    }

    #[test]
    fn dc_and_planar_flat() {
        let r = refs(|_| 100);
        let mut out = [0u16; 64];
        predict(&r, 1, 8, true, 8, &mut out, 8);
        assert!(out.iter().all(|&v| v == 100));
        predict(&r, 0, 8, true, 8, &mut out, 8);
        assert!(out.iter().all(|&v| v == 100));
    }

    #[test]
    fn pure_vertical_and_horizontal() {
        let r = refs(|i| (128 + i) as u16);
        let mut out = [0u16; 16];
        predict(&r, 26, 4, false, 8, &mut out, 4);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(out[y * 4 + x], r.top[1 + x]);
            }
        }
        predict(&r, 10, 4, false, 8, &mut out, 4);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(out[y * 4 + x], r.left[1 + y]);
            }
        }
        // diagonal mode 34 reads top[x + y + 2]
        predict(&r, 34, 4, false, 8, &mut out, 4);
        assert_eq!(out[4 + 1], r.top[1 + 1 + 1 + 1]);
    }

    /// The previous per-sample implementation of planar and angular prediction (8.4.4.2.4, 8.4.4.2.6).
    fn reference(r: &Refs, mode: u32, n: usize, out: &mut [u16]) {
        if mode == 0 {
            let log2 = n.trailing_zeros();
            let (tr, bl) = (r.top[1 + n] as i32, r.left[1 + n] as i32);
            for y in 0..n {
                for x in 0..n {
                    let (l, t) = (r.left[1 + y] as i32, r.top[1 + x] as i32);
                    out[y * n + x] =
                        (((n - 1 - x) as i32 * l + (x as i32 + 1) * tr + (n - 1 - y) as i32 * t + (y as i32 + 1) * bl + n as i32) >> (log2 + 1)) as u16;
                }
            }
            return;
        }
        let angle = INTRA_PRED_ANGLE[mode as usize] as i32;
        let mut refa = [0i32; 3 * 64 + 1];
        let base = n as i32;
        let (main, side) = if mode >= 18 { (&r.top, &r.left) } else { (&r.left, &r.top) };
        for x in 0..=n {
            refa[(base + x as i32) as usize] = main[x] as i32;
        }
        if angle < 0 {
            let lim = (n as i32 * angle) >> 5;
            if lim < -1 {
                let inv = INV_ANGLE[(mode - 11) as usize] as i32;
                for x in lim..=-1 {
                    refa[(base + x) as usize] = side[((x * inv + 128) >> 8) as usize] as i32;
                }
            }
        } else {
            for x in n + 1..=2 * n {
                refa[(base + x as i32) as usize] = main[x] as i32;
            }
        }
        for j in 0..n {
            let pos = (j as i32 + 1) * angle;
            let (idx, fact) = (pos >> 5, pos & 31);
            for i in 0..n {
                let k = (base + i as i32 + idx + 1) as usize;
                let v = if fact != 0 { ((32 - fact) * refa[k] + fact * refa[k + 1] + 16) >> 5 } else { refa[k] };
                if mode < 18 {
                    out[i * n + j] = v as u16;
                } else {
                    out[j * n + i] = v as u16;
                }
            }
        }
    }

    #[test]
    fn planar_and_angular_match_reference() {
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for bit_depth in [8u32, 10, 12] {
            for n in [4usize, 8, 16, 32] {
                for _ in 0..4 {
                    let v: Vec<u16> = (0..257).map(|_| (next() % (1 << bit_depth)) as u16).collect();
                    let r = refs(|i| v[(i + 128) as usize]);
                    for mode in (0..35).filter(|&m| m != 1) {
                        let (mut a, mut b) = (vec![0u16; n * n], vec![0u16; n * n]);
                        predict(&r, mode, n, false, bit_depth, &mut a, n);
                        reference(&r, mode, n, &mut b);
                        assert_eq!(a, b, "n {n} mode {mode} bit depth {bit_depth}");
                    }
                }
            }
        }
    }

    #[test]
    fn substitution() {
        let mut r = Refs::default();
        let mut al = [false; 129];
        let at = [false; 129];
        r.left[3] = 77;
        al[3] = true;
        substitute(&mut r, &al, &at, 4, 8);
        assert!(r.left[..=8].iter().all(|&v| v == 77));
        assert!(r.top[..=8].iter().all(|&v| v == 77));
        let mut r = Refs::default();
        substitute(&mut r, &[false; 129], &[false; 129], 4, 10);
        assert_eq!(r.top[5], 512);
    }
}
