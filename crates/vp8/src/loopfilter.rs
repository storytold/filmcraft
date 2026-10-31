//! Loop filter (15).

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

/// Filter an edge (15.2, 15.3) of `N` segments. `start` is q0 of the first segment; a vertical
/// edge (`vertical`) has its segments one row apart, a horizontal one one pixel apart. Every
/// segment must have 4 pixels inside the plane on both sides of the edge; an edge that does not
/// is left alone.
pub fn filter_edge<const N: usize>(plane: &mut [u8], stride: usize, start: usize, vertical: bool, kind: Kind, prm: &Params) {
    let (step, along) = if vertical { (1, stride) } else { (stride, 1) };
    if start < 4 * step || start + (N - 1) * along + 3 * step >= plane.len() {
        return;
    }
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
