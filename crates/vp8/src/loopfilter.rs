//! Loop filter (15).

use filmcraft_simd::{Vp8Filter, Vp8Limits};

/// Filter parameters of one macroblock (15.4).
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub mbedge_limit: i32,
    pub sub_bedge_limit: i32,
    pub interior_limit: i32,
    pub hev_threshold: i32,
}

impl Params {
    pub fn new(level: u8, sharpness: u8, key_frame: bool) -> Params {
        let level = i32::from(level);
        let mut interior = level;
        if sharpness > 0 {
            interior >>= if sharpness > 4 { 2 } else { 1 };
            interior = interior.min(9 - i32::from(sharpness));
        }
        let interior = interior.max(1);
        let hev_threshold = match (key_frame, level) {
            (_, l) if l >= 40 => 2 + i32::from(!key_frame),
            (false, l) if l >= 20 => 2,
            (_, l) if l >= 15 => 1,
            _ => 0,
        };
        Params { mbedge_limit: (level + 2) * 2 + interior, sub_bedge_limit: level * 2 + interior, interior_limit: interior, hev_threshold }
    }
}

/// Which edge filter to apply along an edge.
#[derive(Clone, Copy)]
pub enum Kind {
    /// The simple filter (15.2) with the macroblock edge limit.
    Simple,
    /// The simple filter with the subblock edge limit.
    SimpleInner,
    /// The normal macroblock edge filter (`MBfilter`, 15.3).
    NormalMb,
    /// The normal subblock edge filter (`subblock_filter`, 15.3).
    NormalSub,
}

/// Filter `N` segments at once: `px[k][l]` is pixel `k - 4` across the edge of segment `l`
/// (p3 p2 p1 p0 | q0 q1 q2 q3). Branch-free: the conditions of 15.2 / 15.3 become masks
/// (all ones or zero) that zero the adjustment where the filter does not apply, which gives the
/// same values as the specification's procedures; each step is a plain loop over the lanes, which
/// the compiler vectorizes.
#[inline(always)]
fn filter_lanes<const N: usize, const NORMAL: bool, const MB: bool>(px: &mut [[u8; N]; 8], edge_limit: i16, prm: &Params) {
    let (lim, thr) = (prm.interior_limit as i16, prm.hev_threshold as i16);
    let ld = |k: usize| -> [i16; N] { std::array::from_fn(|l| i16::from(px[k][l])) };
    let (p3, p2, p1, p0, q0, q1, q2, q3) = (ld(0), ld(1), ld(2), ld(3), ld(4), ld(5), ld(6), ld(7));
    let mut m = [0i16; N];
    let mut hev = [0i16; N];
    for l in 0..N {
        let edge = (p0[l] - q0[l]).abs() * 2 + (p1[l] - q1[l]).abs() / 2;
        m[l] = -i16::from(edge <= edge_limit);
    }
    if NORMAL {
        for l in 0..N {
            let d = (p3[l] - p2[l])
                .abs()
                .max((p2[l] - p1[l]).abs())
                .max((p1[l] - p0[l]).abs())
                .max((q3[l] - q2[l]).abs())
                .max((q2[l] - q1[l]).abs())
                .max((q1[l] - q0[l]).abs());
            m[l] &= -i16::from(d <= lim);
            hev[l] = -i16::from((p1[l] - p0[l]).abs().max((q1[l] - q0[l]).abs()) > thr);
        }
    }
    let c = |v: i16| v.clamp(-128, 127);
    let out = |v: i16| (c(v) + 128) as u8;
    for l in 0..N {
        let (sp2, sp1, sp0, sq0, sq1, sq2) = (p2[l] - 128, p1[l] - 128, p0[l] - 128, q0[l] - 128, q1[l] - 128, q2[l] - 128);
        if !NORMAL {
            // simple_segment / common_adjust with the outer taps
            let f = c(c(sp1 - sq1) + 3 * (sq0 - sp0)) & m[l];
            px[4][l] = out(sq0 - (c(f + 4) >> 3));
            px[3][l] = out(sp0 + (c(f + 3) >> 3));
        } else if !MB {
            // subblock_filter: common_adjust (outer taps on high edge variance), then half of it
            // on p1 / q1 without high edge variance
            let f = c((c(sp1 - sq1) & hev[l]) + 3 * (sq0 - sp0)) & m[l];
            let f1 = c(f + 4) >> 3;
            px[4][l] = out(sq0 - f1);
            px[3][l] = out(sp0 + (c(f + 3) >> 3));
            let a = ((f1 + 1) >> 1) & !hev[l];
            px[5][l] = out(sq1 - a);
            px[2][l] = out(sp1 + a);
        } else {
            // MBfilter: common_adjust with outer taps on high edge variance, the 27/18/9
            // adjustments otherwise
            let w = c(c(sp1 - sq1) + 3 * (sq0 - sp0)) & m[l];
            let f = w & hev[l];
            let q0a = c(sq0 - (c(f + 4) >> 3));
            let p0a = c(sp0 + (c(f + 3) >> 3));
            let w = w & !hev[l];
            // 27 * w + 63 fits 16 bits: |w| <= 128
            let a = |mul: i16| c((mul * w + 63) >> 7);
            let (a27, a18, a9) = (a(27), a(18), a(9));
            px[4][l] = out(q0a - a27);
            px[3][l] = out(p0a + a27);
            px[5][l] = out(sq1 - a18);
            px[2][l] = out(sp1 + a18);
            px[6][l] = out(sq2 - a9);
            px[1][l] = out(sp2 + a9);
        }
    }
}

/// The vector kernels' filter and limits of `kind`.
fn vector_filter(kind: Kind, prm: &Params) -> (Vp8Filter, Vp8Limits) {
    let (f, edge) = match kind {
        Kind::Simple => (Vp8Filter::Simple, prm.mbedge_limit),
        Kind::SimpleInner => (Vp8Filter::Simple, prm.sub_bedge_limit),
        Kind::NormalMb => (Vp8Filter::Macroblock, prm.mbedge_limit),
        Kind::NormalSub => (Vp8Filter::Subblock, prm.sub_bedge_limit),
    };
    (f, Vp8Limits { edge, interior: prm.interior_limit, hev_threshold: prm.hev_threshold })
}

/// [`filter_edge`] of the same 8-segment edge of both chroma planes (VP8 filters them alike): in
/// one call of the vector kernel when it runs.
pub fn filter_edge_uv(u: &mut [u8], v: &mut [u8], stride: usize, start: usize, vertical: bool, kind: Kind, prm: &Params) {
    let (f, limits) = vector_filter(kind, prm);
    if filmcraft_simd::vp8_loop_filter_uv(u, v, (start, stride), vertical, f, &limits) {
        return;
    }
    filter_edge::<8>(u, stride, start, vertical, kind, prm);
    filter_edge::<8>(v, stride, start, vertical, kind, prm);
}

/// Filter an edge (15.2, 15.3) of `N` segments. `start` is q0 of the first segment; a vertical
/// edge (`vertical`) has its segments one row apart, a horizontal one one pixel apart. Every
/// segment must have 4 pixels inside the plane on both sides of the edge; an edge that does not
/// is left alone.
pub fn filter_edge<const N: usize>(plane: &mut [u8], stride: usize, start: usize, vertical: bool, kind: Kind, prm: &Params) {
    let (step, along) = if vertical { (1, stride) } else { (stride, 1) };
    if start < 4 * step || start + (N - 1) * along + 3 * step >= plane.len() {
        return;
    }
    if N == 16 {
        let (f, limits) = vector_filter(kind, prm);
        if filmcraft_simd::vp8_loop_filter(plane, start, stride, vertical, f, &limits) {
            return;
        }
    }
    scalar_edge::<N>(plane, stride, start, vertical, kind, prm);
}

/// [`filter_edge`] by [`filter_lanes`] (the lines are known to fit).
fn scalar_edge<const N: usize>(plane: &mut [u8], stride: usize, start: usize, vertical: bool, kind: Kind, prm: &Params) {
    let step = if vertical { 1 } else { stride };
    let mut px = [[0u8; N]; 8];
    let first = start - 4 * step;
    if vertical {
        for l in 0..N {
            let row = first + l * stride;
            for k in 0..8 {
                px[k][l] = plane[row + k];
            }
        }
    } else {
        for (k, row) in px.iter_mut().enumerate() {
            let at = first + k * stride;
            row.copy_from_slice(&plane[at..at + N]);
        }
    }
    match kind {
        Kind::Simple => filter_lanes::<N, false, false>(&mut px, prm.mbedge_limit as i16, prm),
        Kind::SimpleInner => filter_lanes::<N, false, false>(&mut px, prm.sub_bedge_limit as i16, prm),
        Kind::NormalMb => filter_lanes::<N, true, true>(&mut px, prm.mbedge_limit as i16, prm),
        Kind::NormalSub => filter_lanes::<N, true, false>(&mut px, prm.sub_bedge_limit as i16, prm),
    }
    if vertical {
        for l in 0..N {
            let row = first + l * stride;
            for k in 1..7 {
                plane[row + k] = px[k][l];
            }
        }
    } else {
        for (k, row) in px.iter().enumerate().take(7).skip(1) {
            let at = first + k * stride;
            plane[at..at + N].copy_from_slice(row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vector kernels filter as the scalar lanes do: every filter, both edge directions, luma
    /// edges and paired chroma edges, levels and sharpness over their range, on ramps with steps
    /// (filtered, high edge variance and unfiltered lines) and noise.
    #[test]
    fn vector_edges_match_the_scalar_filter() {
        let mut seed = 0x7a3c_51e9_0b2d_8f61u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (w, h) = (40usize, 40usize);
        let (mut ran, mut chroma_ran) = (0, 0);
        for case in 0..6000 {
            let vertical = case % 2 == 0;
            let kind = [Kind::Simple, Kind::SimpleInner, Kind::NormalMb, Kind::NormalSub][(next() % 4) as usize];
            let prm = Params::new((next() % 64) as u8, (next() % 8) as u8, next() % 2 == 0);
            let mut plane = || -> Vec<u8> {
                let base = (next() % 256) as i32;
                let slope = (next() % 9) as i32 - 4;
                let step = [0i32, 3, 10, 30, 90][(next() % 5) as usize];
                let noise = [0u64, 1, 4, 16][(next() % 4) as usize];
                (0..w * h)
                    .map(|i| {
                        let (x, y) = ((i % w) as i32, (i / w) as i32);
                        let a = if vertical { x } else { y };
                        let v = base + slope * a + if a >= 16 { step } else { 0 } + (next() % (noise + 1)) as i32;
                        match next() % 40 {
                            0 => 0,
                            1 => 255,
                            _ => v.clamp(0, 255) as u8,
                        }
                    })
                    .collect()
            };
            let (y0, u0, v0) = (plane(), plane(), plane());
            // the edge between columns (rows) 15 and 16, lines 8..24 (luma) and 8..16 (chroma)
            let start = if vertical { 8 * w + 16 } else { 16 * w + 8 };
            let (f, limits) = vector_filter(kind, &prm);
            let mut want = y0.clone();
            scalar_edge::<16>(&mut want, w, start, vertical, kind, &prm);
            let (mut wu, mut wv) = (u0.clone(), v0.clone());
            scalar_edge::<8>(&mut wu, w, start, vertical, kind, &prm);
            scalar_edge::<8>(&mut wv, w, start, vertical, kind, &prm);
            for &level in filmcraft_simd::levels() {
                let mut got = y0.clone();
                if filmcraft_simd::vp8_loop_filter_at(level, &mut got, start, w, vertical, f, &limits) {
                    ran += 1;
                    assert_eq!(got, want, "{level:?} luma case {case} vertical {vertical} {f:?} {prm:?}");
                } else {
                    assert_eq!(got, y0, "nothing written when declined");
                }
                let (mut gu, mut gv) = (u0.clone(), v0.clone());
                if filmcraft_simd::vp8_loop_filter_uv_at(level, &mut gu, &mut gv, (start, w), vertical, f, &limits) {
                    chroma_ran += 1;
                    assert_eq!((gu, gv), (wu.clone(), wv.clone()), "{level:?} chroma case {case} vertical {vertical} {f:?} {prm:?}");
                }
            }
        }
        // (only aarch64 has a vector version so far)
        assert!(ran > 0 || !cfg!(target_arch = "aarch64"));
        assert!(chroma_ran > 0 || !cfg!(target_arch = "aarch64"));
    }
}
