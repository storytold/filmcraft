//! Deblocking filter (8.7) for progressive frames: 4:2:0 and 4:2:2 chroma, 8- to 14-bit samples.
//!
//! Samples are `u16` at every bit depth (narrowed when the picture is output). The α′/β′/t′C0
//! tables (8-16/8-17) are indexed with the raw QPY / QPC — **without** QpBdOffset — and their
//! values scale with the component's bit depth (8-453..8-459, 8-461/8-462); adjusted samples clip
//! with Clip1Y / Clip1C (5-3/5-4, applied at 8-468/8-469). Chroma edges follow ChromaArrayType
//! (8.7 step 3.e): 4:2:0 filters the edges at xE = 0, 4 and yE = 0, 4 of the 8x8 chroma macroblock;
//! 4:2:2 also filters the horizontal edges at yE = 8 and 12 of its 8x16 chroma macroblock. Every
//! chroma edge uses the boundary strength of the co-located luma edge (8.7.2).
//!
//! ITU-T Rec. H.264 (ISO/IEC 14496-10), edition 04/2017: clauses 8.7, 8.7.1, 8.7.2, 8.7.2.2–8.7.2.4,
//! equations 8-450..8-487, Tables 8-16/8-17.

use crate::picture::{Format, MbKind, MbState, Planes};
use crate::slicedec::{PicState, SliceInfo};
use crate::tables::{ALPHA, BETA, TC0};

/// Reference picture identity + mvs of one 4x4 block, for bS = 1 decisions.
#[derive(Clone, Copy)]
struct Motion {
    ids: [u32; 2],
    mv: [[i16; 2]; 2],
    count: u8,
}

/// Referenced picture ids per 8x8 block and list (u32::MAX = list unused).
fn ref_ids8(st: &MbState, sl: &SliceInfo) -> [[u32; 2]; 4] {
    std::array::from_fn(|b8| {
        std::array::from_fn(|l| {
            let r = st.ref_idx[l][b8];
            if r >= 0 { sl.ref_ids[l].get(r as usize).copied().unwrap_or(u32::MAX - 1) } else { u32::MAX }
        })
    })
}

#[inline(always)]
fn block_motion(st: &MbState, ids8: &[[u32; 2]; 4], raster: usize) -> Motion {
    let b8 = (raster >> 3) * 2 + ((raster & 3) >> 1);
    let ids = ids8.get(b8).copied().unwrap_or([u32::MAX; 2]);
    let count = (ids[0] != u32::MAX) as u8 + (ids[1] != u32::MAX) as u8;
    Motion { ids, mv: [st.mv[0].get(raster).copied().unwrap_or([0; 2]), st.mv[1].get(raster).copied().unwrap_or([0; 2])], count }
}

#[inline(always)]
fn mv_far(a: [i16; 2], b: [i16; 2]) -> bool {
    (a[0] as i32 - b[0] as i32).abs() >= 4 || (a[1] as i32 - b[1] as i32).abs() >= 4
}

#[inline]
fn motion_bs(p: &Motion, q: &Motion) -> u8 {
    if p.ids == q.ids && p.mv == q.mv {
        return 0;
    }
    if p.count != q.count {
        return 1;
    }
    if p.count == 1 {
        let (pi, pm) = if p.ids[0] != u32::MAX { (p.ids[0], p.mv[0]) } else { (p.ids[1], p.mv[1]) };
        let (qi, qm) = if q.ids[0] != u32::MAX { (q.ids[0], q.mv[0]) } else { (q.ids[1], q.mv[1]) };
        if pi != qi {
            return 1;
        }
        return mv_far(pm, qm) as u8;
    }
    if p.count == 0 {
        return 0;
    }
    // two motion vectors each
    let same_set = (p.ids[0] == q.ids[0] && p.ids[1] == q.ids[1]) || (p.ids[0] == q.ids[1] && p.ids[1] == q.ids[0]);
    if !same_set {
        return 1;
    }
    if p.ids[0] != p.ids[1] {
        // two different reference pictures: compare mvs referring to the same picture
        if p.ids[0] == q.ids[0] {
            (mv_far(p.mv[0], q.mv[0]) || mv_far(p.mv[1], q.mv[1])) as u8
        } else {
            (mv_far(p.mv[0], q.mv[1]) || mv_far(p.mv[1], q.mv[0])) as u8
        }
    } else {
        // both mvs refer to the same picture
        ((mv_far(p.mv[0], q.mv[0]) || mv_far(p.mv[1], q.mv[1])) && (mv_far(p.mv[0], q.mv[1]) || mv_far(p.mv[1], q.mv[0]))) as u8
    }
}

/// qP with which the α/β/tC0 tables are indexed (8.7.2.2: "qP is set to the value of QPY" / of
/// QPC — i.e. the raw quantisation parameter **without** QpBdOffset; the table values are scaled
/// to the bit depth separately). `eff` is the effective qP of [`MbState`] (QPY + QpBdOffsetY or
/// QPC + QpBdOffsetC), `qpbd` the offset to subtract back. An I_PCM macroblock uses qP 0.
#[inline]
fn luma_table_qp(st: &MbState, qpbd: i32) -> i32 {
    if st.kind == MbKind::IPcm { 0 } else { st.qp as i32 - qpbd }
}

/// Same for chroma: the QPC of the macroblock (QPC of QPY = 0 for I_PCM, already in the state).
/// May be negative (QPC ranges −QpBdOffsetC..39, 8.5.8 NOTE 1); the table index clips it.
#[inline]
fn chroma_table_qp(st: &MbState, c: usize, qpbd: i32) -> i32 {
    st.qpc.get(c).copied().unwrap_or(0) as i32 - qpbd
}

/// Filter one set of eight samples p3 p2 p1 p0 | q0 q1 q2 q3 across an edge (8.7.2.3 / 8.7.2.4):
/// the per-line reference for [`filter_lanes`]. `max` is the Clip1 bound (0..=max).
#[cfg(test)]
fn filter8(v: &mut [i32; 8], bs: u8, alpha: i32, beta: i32, tc0: i32, chroma: bool, max: i32) {
    let p0 = v[3];
    let q0 = v[4];
    let p1 = v[2];
    let q1 = v[5];
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    if chroma {
        if bs < 4 {
            let tc = tc0 + 1;
            let delta = (-tc).max((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3).min(tc);
            v[3] = (p0 + delta).clamp(0, max);
            v[4] = (q0 - delta).clamp(0, max);
        } else {
            v[3] = ((2 * p1 + p0 + q1 + 2) >> 2).clamp(0, max);
            v[4] = ((2 * q1 + q0 + p1 + 2) >> 2).clamp(0, max);
        }
        return;
    }
    let p2 = v[1];
    let q2 = v[6];
    let ap = (p2 - p0).abs();
    let aq = (q2 - q0).abs();
    if bs < 4 {
        let tc = tc0 + (ap < beta) as i32 + (aq < beta) as i32;
        let delta = (-tc).max((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3).min(tc);
        v[3] = (p0 + delta).clamp(0, max);
        v[4] = (q0 - delta).clamp(0, max);
        if ap < beta {
            v[2] = (p1 + (-tc0).max((p2 + ((p0 + q0 + 1) >> 1) - (p1 << 1)) >> 1).min(tc0)).clamp(0, max);
        }
        if aq < beta {
            v[5] = (q1 + (-tc0).max((q2 + ((p0 + q0 + 1) >> 1) - (q1 << 1)) >> 1).min(tc0)).clamp(0, max);
        }
    } else {
        let strong = (p0 - q0).abs() < ((alpha >> 2) + 2);
        if ap < beta && strong {
            let p3 = v[0];
            v[3] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3).clamp(0, max);
            v[2] = ((p2 + p1 + p0 + q0 + 2) >> 2).clamp(0, max);
            v[1] = ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3).clamp(0, max);
        } else {
            v[3] = ((2 * p1 + p0 + q1 + 2) >> 2).clamp(0, max);
        }
        if aq < beta && strong {
            let q3 = v[7];
            v[4] = ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3).clamp(0, max);
            v[5] = ((p0 + q0 + q1 + q2 + 2) >> 2).clamp(0, max);
            v[6] = ((2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3).clamp(0, max);
        } else {
            v[4] = ((2 * q1 + q0 + p1 + 2) >> 2).clamp(0, max);
        }
    }
}

/// Edge filter parameters for `n` lines; `bs[i]` / `tc0[i]` apply to lines `i * n / 4 .. (i + 1) * n / 4`.
struct EdgeParams {
    bs: [u8; 4],
    tc0: [i32; 4],
    alpha: i32,
    beta: i32,
}

/// α, β and tC0 of one edge (8.7.2.2 / 8.7.2.3): `qPav = ( qPp + qPq + 1 ) >> 1` (8-453),
/// `indexA = Clip3( 0, 51, qPav + filterOffsetA )` / `indexB = Clip3( 0, 51, qPav + filterOffsetB )`
/// (8-454/8-455) index the 8-bit tables, whose values scale by `1 << shift` — `BitDepthY − 8` for
/// luma (8-456/8-457, 8-461), `BitDepthC − 8` for chroma (8-458/8-459, 8-462). `qp0` / `qp1` are
/// the raw qP values of the two macroblocks at the edge ([`luma_table_qp`] / [`chroma_table_qp`]);
/// `off_a` / `off_b` are the doubled slice offsets (SliceAlphaC0OffsetDiv2 × 2 and
/// SliceBetaOffsetDiv2 × 2). tC0 of bS = 4 is not used (its filters have no tC).
fn edge_params(qp0: i32, qp1: i32, bs: [u8; 4], off_a: i32, off_b: i32, shift: u32) -> EdgeParams {
    let shift = shift.min(8); // bit depths are capped at 16 (Format::max_y / max_c)
    let qpav = (qp0 + qp1 + 1) >> 1;
    let index_a = (qpav + off_a).clamp(0, 51) as usize;
    let index_b = (qpav + off_b).clamp(0, 51) as usize;
    let tc = |b: u8| if (1..4).contains(&b) { (TC0[index_a].get(b as usize - 1).copied().unwrap_or(0) as i32) << shift } else { 0 };
    EdgeParams { bs, tc0: [tc(bs[0]), tc(bs[1]), tc(bs[2]), tc(bs[3])], alpha: (ALPHA[index_a] as i32) << shift, beta: (BETA[index_b] as i32) << shift }
}

/// Filter one edge for `N` lines at once (lane `i` = line `i` across the edge). `s[k][i]` holds
/// sample p3 p2 p1 p0 q0 q1 q2 q3 (k = 0..8) of line `i`, `max` is the Clip1 bound. Branch-free
/// per lane, so the loops vectorise; bit-exact with [`filter8`] applied to every line
/// (8.7.2.3 / 8.7.2.4).
#[inline(always)]
fn filter_lanes<const N: usize>(s: &mut [[i32; N]; 8], ep: &EdgeParams, chroma: bool, max: i32) {
    let per = N / 4;
    let (alpha, beta) = (ep.alpha, ep.beta);
    let mut bs_on = [false; N];
    let mut tc0 = [0i32; N];
    for k in 0..4 {
        for i in k * per..(k + 1) * per {
            bs_on[i] = ep.bs[k] != 0;
            tc0[i] = ep.tc0[k];
        }
    }
    let [p3, p2, p1, p0, q0, q1, q2, q3] = *s;
    // Masks are 0 / -1 per lane, selects are bitwise, and clipping to per-lane bounds uses
    // min / max (`clamp` would assert its bounds per lane), so every loop below vectorises.
    let lt = |x: i32, y: i32| -((x < y) as i32);
    let sel = |m: i32, a: i32, b: i32| (a & m) | (b & !m);
    let clip = |v: i32| v.max(0).min(max);
    let mut on = [0i32; N];
    let mut any = 0i32;
    for i in 0..N {
        on[i] = -(bs_on[i] as i32) & lt((p0[i] - q0[i]).abs(), alpha) & lt((p1[i] - p0[i]).abs(), beta) & lt((q1[i] - q0[i]).abs(), beta);
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
                ((p0[i] + delta).clamp(0, max), (q0[i] - delta).clamp(0, max))
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
            s[3][i] = sel(sp, clip(sp0), sel(on[i], clip(wp0), d));
            s[2][i] = sel(sp, clip((b + c + d + e + 2) >> 2), c);
            s[1][i] = sel(sp, clip((2 * a + 3 * b + c + d + e + 4) >> 3), b);
            s[4][i] = sel(sq, clip(sq0), sel(on[i], clip(wq0), e));
            s[5][i] = sel(sq, clip((d + e + f + g + 2) >> 2), f);
            s[6][i] = sel(sq, clip((2 * h + 3 * g + f + e + d + 4) >> 3), g);
        }
    } else {
        for i in 0..N {
            let ap = lt((p2[i] - p0[i]).abs(), beta);
            let aq = lt((q2[i] - q0[i]).abs(), beta);
            let t0 = tc0[i];
            let tc = t0 - ap - aq;
            let delta = ((((q0[i] - p0[i]) << 2) + (p1[i] - q1[i]) + 4) >> 3).max(-tc).min(tc);
            let avg = (p0[i] + q0[i] + 1) >> 1;
            let np1 = clip(p1[i] + ((p2[i] + avg - (p1[i] << 1)) >> 1).max(-t0).min(t0));
            let nq1 = clip(q1[i] + ((q2[i] + avg - (q1[i] << 1)) >> 1).max(-t0).min(t0));
            s[3][i] = sel(on[i], clip(p0[i] + delta), p0[i]);
            s[4][i] = sel(on[i], clip(q0[i] - delta), q0[i]);
            s[2][i] = sel(on[i] & ap, np1, p1[i]);
            s[5][i] = sel(on[i] & aq, nq1, q1[i]);
        }
    }
}

/// Filter a vertical edge whose q0 column is at `x`, for `N` lines starting at row `y`.
#[inline(always)]
fn filter_vertical<const N: usize>(pix: &mut [u16], stride: usize, x: usize, y: usize, ep: &EdgeParams, chroma: bool, max: i32) {
    let Some(x0) = x.checked_sub(4) else {
        return;
    };
    // p3 .. q3 of every line, gathered before anything is written back.
    let mut s = [[0i32; N]; 8];
    for i in 0..N {
        let row = match (y + i).checked_mul(stride).and_then(|o| pix.get(o + x0..)).and_then(|p| p.first_chunk::<8>()) {
            Some(r) => r,
            None => return,
        };
        for (k, row_v) in row.iter().enumerate() {
            s[k][i] = *row_v as i32;
        }
    }
    filter_lanes(&mut s, ep, chroma, max);
    for i in 0..N {
        let o = match (y + i).checked_mul(stride).and_then(|o| o.checked_add(x0)).and_then(|o| pix.get_mut(o..)).and_then(|p| p.first_chunk_mut::<8>()) {
            Some(r) => r,
            None => return,
        };
        for (k, v) in o.iter_mut().enumerate() {
            *v = s[k][i].clamp(0, 0xffff) as u16;
        }
    }
}

/// Filter a horizontal edge whose q0 row is `y`, for `N` columns starting at `x`.
#[inline(always)]
fn filter_horizontal<const N: usize>(pix: &mut [u16], stride: usize, x: usize, y: usize, ep: &EdgeParams, chroma: bool, max: i32) {
    let Some(y0) = y.checked_sub(4) else {
        return;
    };
    let base = match y0.checked_mul(stride).and_then(|o| o.checked_add(x)) {
        Some(b) => b,
        None => return,
    };
    let mut s = [[0i32; N]; 8];
    for (k, row) in s.iter_mut().enumerate() {
        let Some(src) = base.checked_add(k * stride).and_then(|o| pix.get(o..)).and_then(|p| p.first_chunk::<N>()) else {
            return;
        };
        for i in 0..N {
            row[i] = src[i] as i32;
        }
    }
    filter_lanes(&mut s, ep, chroma, max);
    let (k0, k1) = if chroma { (3, 5) } else { (1, 7) };
    for (k, row) in s.iter().enumerate().take(k1).skip(k0) {
        let Some(dst) = base.checked_add(k * stride).and_then(|o| pix.get_mut(o..)).and_then(|p| p.first_chunk_mut::<N>()) else {
            return;
        };
        for i in 0..N {
            dst[i] = row[i].clamp(0, 0xffff) as u16;
        }
    }
}

/// Chroma edges of one macroblock, per 8.7 step 3.e: vertical edges at xE = 0 and 4 (MbWidthC = 8
/// in both 4:2:0 and 4:2:2), horizontal edges every 4 rows of MbHeightC = 16 >> chroma_y_shift —
/// yE = 0, 4 in 4:2:0 and yE = 0, 4, 8, 12 in 4:2:2. Hostile `fmt` shifts are clamped to 0 / 1.
fn chroma_edge_count(fmt: Format, vertical: bool) -> usize {
    let cy = fmt.chroma_y_shift.min(1);
    if vertical { 2 } else { 4 >> cy }
}

/// Luma edge whose boundary strength applies to chroma edge `ce` (8.7.2: the chroma edge at
/// ( xE, yE ) uses the bS of the co-located luma edge at ( SubWidthC * xE, SubHeightC * yE )).
fn chroma_luma_edge(fmt: Format, vertical: bool, ce: usize) -> usize {
    let (cx, cy) = (fmt.chroma_x_shift.min(1), fmt.chroma_y_shift.min(1));
    ce << if vertical { cx } else { cy }
}

/// Deblock one macroblock (macroblocks must be processed in raster order).
pub fn deblock_mb(pic: &mut PicState, addr: usize, mb_w: usize, fmt: Format) {
    deblock_mb_planes(&mut pic.planes, &pic.mbs, &pic.slices, addr, mb_w, fmt);
}

/// The work of [`deblock_mb`] on plain buffers (also the unit-test entry point).
#[allow(clippy::needless_range_loop)]
fn deblock_mb_planes(planes: &mut Planes, mbs: &[MbState], slices: &[SliceInfo], addr: usize, mb_w: usize, fmt: Format) {
    if mb_w == 0 {
        return;
    }
    let Some(q) = mbs.get(addr) else {
        return;
    };
    if q.slice_num == u32::MAX {
        return;
    }
    let Some(sq) = slices.get(q.slice_num as usize) else {
        return;
    };
    if sq.disable_deblocking_filter_idc == 1 {
        return;
    }
    let (mx, my) = (addr % mb_w, addr / mb_w);
    let usable = |n: Option<usize>| -> Option<usize> {
        let n = n?;
        let st = mbs.get(n)?;
        if st.slice_num == u32::MAX || (sq.disable_deblocking_filter_idc == 2 && st.slice_num != q.slice_num) {
            return None;
        }
        Some(n)
    };
    let left = usable(if mx > 0 { Some(addr - 1) } else { None });
    let top = usable(if my > 0 { Some(addr - mb_w) } else { None });
    let alpha_off = sq.alpha_offset;
    let beta_off = sq.beta_offset;
    let t8 = q.transform_8x8;
    // bS[dir][edge][segment]; dir 0 = vertical edges (x), 1 = horizontal edges (y)
    let mut bs = [[[0u8; 4]; 4]; 2];
    if q.kind.is_intra() {
        for dir in 0..2 {
            if (if dir == 0 { left } else { top }).is_some() {
                bs[dir][0] = [4; 4];
            }
            for e in 1..4 {
                // bS is derived for every edge (8.7.2.1): the transform_size_8x8 edge selection
                // below skips odd *luma* edges only; 4:2:2 chroma edges still use these bS values.
                bs[dir][e] = [3; 4];
            }
        }
    } else {
        let qids = ref_ids8(q, sq);
        let q_uniform = (0..2).all(|l| {
            let r = q.ref_idx[l];
            r[1] == r[0] && r[2] == r[0] && r[3] == r[0] && q.mv[l].iter().all(|m| *m == q.mv[l][0])
        });
        let qm: [Motion; 16] = std::array::from_fn(|r| block_motion(q, &qids, r));
        for dir in 0..2 {
            let neighbor = if dir == 0 { left } else { top };
            if let Some(n) = neighbor
                && let Some(p) = mbs.get(n)
            {
                if p.kind.is_intra() {
                    bs[dir][0] = [4; 4];
                } else if let Some(ps) = slices.get(p.slice_num as usize) {
                    let pids = ref_ids8(p, ps);
                    for k in 0..4 {
                        let (rq, rp) = if dir == 0 { (k * 4, k * 4 + 3) } else { (k, 12 + k) };
                        bs[dir][0][k] =
                            if (p.nz_mask >> rp) & 1 != 0 || (q.nz_mask >> rq) & 1 != 0 { 2 } else { motion_bs(&block_motion(p, &pids, rp), &qm[rq]) };
                    }
                }
            }
            if q_uniform && q.nz_mask == 0 {
                // one motion for the whole MB and no coefficients: all internal edges have bS 0
                continue;
            }
            for e in 1..4 {
                for k in 0..4 {
                    let (rq, rp) = if dir == 0 { (k * 4 + e, k * 4 + e - 1) } else { (e * 4 + k, e * 4 + k - 4) };
                    bs[dir][e][k] = if (q.nz_mask >> rp) & 1 != 0 || (q.nz_mask >> rq) & 1 != 0 { 2 } else { motion_bs(&qm[rp], &qm[rq]) };
                }
            }
        }
    }
    // QpBdOffsetY / QpBdOffsetC (7-4 / 7-6); the tables index raw QPY / QPC (8.7.2.2).
    let cy = fmt.chroma_y_shift.min(1);
    let bd_y = fmt.bit_depth.min(16).saturating_sub(8);
    let bd_c = fmt.bit_depth_c.min(16).saturating_sub(8);
    let qpbd_y = 6 * bd_y as i32;
    let qpbd_c = 6 * bd_c as i32;
    let (shift_y, shift_c) = (bd_y, bd_c);
    let q_qp = luma_table_qp(q, qpbd_y);
    let q_qpc = [chroma_table_qp(q, 0, qpbd_c), chroma_table_qp(q, 1, qpbd_c)];
    let p_qp =
        [left, top].map(|n| n.and_then(|n| mbs.get(n)).map(|st| (luma_table_qp(st, qpbd_y), [chroma_table_qp(st, 0, qpbd_c), chroma_table_qp(st, 1, qpbd_c)])));
    let width = planes.width;
    let cwidth = planes.cwidth;
    let max_y = fmt.max_y();
    let max_c = fmt.max_c();
    // chroma geometry of one macroblock: MbWidthC = 8 wide, MbHeightC = 16 >> chroma_y_shift high
    let clines = 16usize >> cy;
    // luma
    for dir in 0..2 {
        for e in 0..4 {
            // transform_size_8x8_flag selects the luma edges (8.7): with the 8x8 transform only
            // the solid bold edges (multiples of 8 samples) are filtered. Chroma 4:2:2 uses the
            // odd edges' bS regardless (see below), so the selection is applied here only.
            if t8 && e % 2 == 1 {
                continue;
            }
            if bs[dir][e] == [0; 4] {
                continue;
            }
            let qpp = if e == 0 { p_qp[dir].map(|p| p.0).unwrap_or(q_qp) } else { q_qp };
            let ep = edge_params(qpp, q_qp, bs[dir][e], alpha_off, beta_off, shift_y);
            if ep.alpha == 0 || ep.beta == 0 {
                // filterSamplesFlag needs |p0 - q0| < alpha and |p1 - p0| < beta (8.7.2.2): no
                // sample of the edge changes (low QPs)
                continue;
            }
            if dir == 0 {
                filter_vertical::<16>(&mut planes.y, width, mx * 16 + e * 4, my * 16, &ep, false, max_y);
            } else {
                filter_horizontal::<16>(&mut planes.y, width, mx * 16, my * 16 + e * 4, &ep, false, max_y);
            }
        }
    }
    // chroma: every edge uses the bS of the co-located luma edge (8.7.2). In 4:2:0 the vertical
    // edges at xE = 0, 4 and horizontal edges at yE = 0, 4 map to luma edges 0, 2; in 4:2:2 the
    // horizontal edges at yE = 0, 4, 8, 12 map to luma edges 0, 1, 2, 3 (SubHeightC = 1).
    for c in 0..2 {
        for dir in 0..2 {
            let vertical = dir == 0;
            for ce in 0..chroma_edge_count(fmt, vertical) {
                let e = chroma_luma_edge(fmt, vertical, ce);
                let Some(edge_bs) = bs[dir].get(e) else {
                    continue;
                };
                if *edge_bs == [0; 4] {
                    continue;
                }
                let qpp = if e == 0 { p_qp[dir].map(|p| p.1[c]).unwrap_or(q_qpc[c]) } else { q_qpc[c] };
                let ep = edge_params(qpp, q_qpc[c], *edge_bs, alpha_off, beta_off, shift_c);
                if ep.alpha == 0 || ep.beta == 0 {
                    continue;
                }
                let plane = if c == 0 { &mut planes.cb } else { &mut planes.cr };
                if vertical {
                    if clines == 16 {
                        filter_vertical::<16>(plane, cwidth, mx * 8 + ce * 4, my * 16, &ep, true, max_c);
                    } else {
                        filter_vertical::<8>(plane, cwidth, mx * 8 + ce * 4, my * clines, &ep, true, max_c);
                    }
                } else {
                    filter_horizontal::<8>(plane, cwidth, mx * 8, my * clines + ce * 4, &ep, true, max_c);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v8_420() -> Format {
        Format::V8_420
    }
    fn f10_422() -> Format {
        Format { chroma_x_shift: 1, chroma_y_shift: 0, bit_depth: 10, bit_depth_c: 10 }
    }
    fn f10_420() -> Format {
        Format { chroma_x_shift: 1, chroma_y_shift: 1, bit_depth: 10, bit_depth_c: 10 }
    }

    /// The vectorised edge filter equals the per-line reference [`filter8`] on random edges
    /// (small differences across the edge so that every filter branch is taken), at several bit
    /// depths (the Clip1 bound is `max`).
    #[test]
    fn lane_filter_matches_per_line_reference() {
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for iter in 0..20_000 {
            let chroma = iter % 3 == 0;
            let strong = iter % 4 == 1;
            let max = [255, 1023, 4095][iter % 3];
            let bs: [u8; 4] = if strong { [4; 4] } else { std::array::from_fn(|_| (next() % 4) as u8) };
            let index_a = (next() % 52) as usize;
            let index_b = (next() % 52) as usize;
            let tc = |b: u8| if (1..4).contains(&b) { TC0[index_a][b as usize - 1] as i32 } else { 0 };
            let ep = EdgeParams { bs, tc0: bs.map(tc), alpha: ALPHA[index_a] as i32, beta: BETA[index_b] as i32 };
            let base = (next() % max as u64) as i32;
            let spread = 1 + (next() % 24) as i32;
            let mut lines = [[0i32; 8]; 16];
            for l in lines.iter_mut() {
                for v in l.iter_mut() {
                    *v = (base + (next() % (2 * spread as u64 + 1)) as i32 - spread).clamp(0, max);
                }
            }
            let n = if chroma { 8 } else { 16 };
            let mut want = lines;
            for (i, l) in want.iter_mut().enumerate().take(n) {
                let k = i / (n / 4);
                if bs[k] != 0 {
                    filter8(l, bs[k], ep.alpha, ep.beta, ep.tc0[k], chroma, max);
                }
            }
            let got: Vec<[i32; 8]> = if chroma {
                let mut s = [[0i32; 8]; 8];
                for i in 0..8 {
                    for k in 0..8 {
                        s[k][i] = lines[i][k];
                    }
                }
                filter_lanes(&mut s, &ep, true, max);
                (0..8).map(|i| std::array::from_fn(|k| s[k][i])).collect()
            } else {
                let mut s = [[0i32; 16]; 8];
                for i in 0..16 {
                    for k in 0..8 {
                        s[k][i] = lines[i][k];
                    }
                }
                filter_lanes(&mut s, &ep, false, max);
                (0..16).map(|i| std::array::from_fn(|k| s[k][i])).collect()
            };
            assert_eq!(&got[..], &want[..n], "iteration {iter}: bs {bs:?} indexA {index_a} indexB {index_b} max {max}");
        }
    }

    /// Thresholds are looked up in the 8-bit tables with the raw qP and scaled by 1 << (bit depth
    /// − 8) (8-456..8-459, 8-461/8-462); the table indices clip to 0..=51 (8-454/8-455).
    #[test]
    fn thresholds_scale_with_bit_depth() {
        let bs3 = [3u8; 4];
        let p8 = edge_params(30, 30, bs3, 0, 0, 0);
        assert_eq!(p8.alpha, ALPHA[30] as i32, "8-bit: alpha is the table value");
        assert_eq!(p8.beta, BETA[30] as i32);
        assert_eq!(p8.tc0[0], TC0[30][2] as i32, "bS 3 indexes the third column");
        // 10-bit: table values x4 (1 << (10 − 8)), indices unchanged
        let p10 = edge_params(30, 30, bs3, 0, 0, 2);
        assert_eq!(p10.alpha, 4 * p8.alpha);
        assert_eq!(p10.beta, 4 * p8.beta);
        assert_eq!(p10.tc0, p8.tc0.map(|t| t * 4));
        // 12-bit: x16
        let p12 = edge_params(30, 30, bs3, 0, 0, 4);
        assert_eq!(p12.alpha, 16 * p8.alpha);
        // qPav = (qPp + qPq + 1) >> 1 (8-453), indices clip at both ends (8-454/8-455)
        assert_eq!(edge_params(10, 11, bs3, 0, 0, 0).alpha, ALPHA[11] as i32, "qPav of 10/11 is 11");
        assert_eq!(edge_params(20, 21, bs3, 100, 100, 0).alpha, ALPHA[51] as i32, "index clips at 51");
        assert_eq!(edge_params(-24, -24, bs3, 0, 0, 0).alpha, ALPHA[0] as i32, "negative raw qP clips at 0");
        // bS 4 has no tC0 (its filters do not use one)
        let p4 = edge_params(30, 30, [4; 4], 0, 0, 2);
        assert_eq!(p4.tc0, [0; 4]);
    }

    /// Chroma edge layout per chroma format (8.7 step 3.e) and the luma edge each takes bS from
    /// (8.7.2: co-located at SubWidthC * x, SubHeightC * y).
    #[test]
    fn chroma_edge_layout_420_and_422() {
        // 4:2:0: vertical xE = 0, 4 -> luma edges 0, 2; horizontal yE = 0, 4 -> luma edges 0, 2
        assert_eq!(chroma_edge_count(v8_420(), true), 2);
        assert_eq!(chroma_edge_count(v8_420(), false), 2);
        assert_eq!((0..2).map(|ce| chroma_luma_edge(v8_420(), true, ce)).collect::<Vec<_>>(), [0, 2]);
        assert_eq!((0..2).map(|ce| chroma_luma_edge(v8_420(), false, ce)).collect::<Vec<_>>(), [0, 2]);
        // 4:2:2: same vertical edges; horizontal yE = 0, 4, 8, 12 -> luma edges 0, 1, 2, 3
        assert_eq!(chroma_edge_count(f10_422(), true), 2);
        assert_eq!(chroma_edge_count(f10_422(), false), 4);
        assert_eq!((0..2).map(|ce| chroma_luma_edge(f10_422(), true, ce)).collect::<Vec<_>>(), [0, 2]);
        assert_eq!((0..4).map(|ce| chroma_luma_edge(f10_422(), false, ce)).collect::<Vec<_>>(), [0, 1, 2, 3]);
    }

    /// Two MB rows x two MB columns of intra macroblocks, every chroma row group flat with an
    /// 8-sample step at every 4-row boundary: filtering touches exactly the p0/q0 pairs of the
    /// chroma edges (the chroma filter never changes p1/q1/p2/q2).
    fn grid(fmt: Format) -> (Planes, Vec<MbState>, Vec<SliceInfo>) {
        let (mb_w, mb_h) = (2, 2);
        let mut planes = Planes::new(mb_w * 16, mb_h * 16, fmt);
        let mid = 1i32 << (fmt.bit_depth.min(16) - 1);
        let qpe = (51 + 6 * fmt.bit_depth.min(16).saturating_sub(8)) as u8; // effective QP of raw 51
        for c in [&mut planes.cb, &mut planes.cr] {
            for (i, v) in c.iter_mut().enumerate() {
                let r = i / planes.cwidth;
                *v = (mid + 8 * ((r / 4) % 2) as i32) as u16;
            }
        }
        for (i, v) in planes.y.iter_mut().enumerate() {
            let r = i / planes.width;
            *v = (mid + 8 * ((r / 4) % 2) as i32) as u16;
        }
        let mut mbs = vec![MbState::default(); mb_w * mb_h];
        for st in &mut mbs {
            st.slice_num = 0;
            st.kind = MbKind::I16x16;
            st.qp = qpe;
            st.qpc = [qpe, qpe];
        }
        let slices = vec![SliceInfo { disable_deblocking_filter_idc: 0, alpha_offset: 0, beta_offset: 0, ref_ids: [Vec::new(), Vec::new()] }];
        (planes, mbs, slices)
    }

    /// Rows of `plane` (columns x..x+w) that changed against `before`.
    fn changed_rows(now: &[u16], before: &[u16], stride: usize, x: usize, w: usize) -> Vec<usize> {
        let rows = now.len() / stride;
        (0..rows).filter(|&r| (x..x + w).any(|c| now[r * stride + c] != before[r * stride + c])).collect()
    }

    /// 4:2:2 filters the horizontal chroma edges at yE = 0, 4, 8, 12 of the 8x16 chroma
    /// macroblock (8.7 step 3.e): the p0/q0 row pair of every one of the four edges changes, and
    /// nothing else.
    #[test]
    fn chroma_422_filters_horizontal_edges_every_four_rows() {
        let fmt = f10_422();
        let (mut planes, mbs, slices) = grid(fmt);
        let before = planes.cb.clone();
        // MB (1,1): chroma rows 16..32, columns 8..16; edges at rows 16, 20, 24, 28
        deblock_mb_planes(&mut planes, &mbs, &slices, 3, 2, fmt);
        assert_eq!(changed_rows(&planes.cb, &before, planes.cwidth, 8, 8), [15, 16, 19, 20, 23, 24, 27, 28], "4:2:2: p0/q0 of the edges at yE = 0, 4, 8, 12");
    }

    /// 4:2:0 filters only yE = 0 and 4 of its 8x8 chroma macroblock (8.7 step 3.e).
    #[test]
    fn chroma_420_filters_horizontal_edges_every_eight_rows() {
        let fmt = f10_420();
        let (mut planes, mbs, slices) = grid(fmt);
        let before = planes.cb.clone();
        // MB (1,1): chroma rows 8..16, columns 8..16; edges at rows 8 and 12
        deblock_mb_planes(&mut planes, &mbs, &slices, 3, 2, fmt);
        assert_eq!(changed_rows(&planes.cb, &before, planes.cwidth, 8, 8), [7, 8, 11, 12], "4:2:0: p0/q0 of the edges at yE = 0, 4");
    }

    /// The luma filter still runs on the four 4-line edges of the macroblock (smoke test through
    /// the full bS + qP plumbing at 8-bit).
    #[test]
    fn luma_edges_still_filter_at_8_bit() {
        let fmt = v8_420();
        let (mut planes, mbs, slices) = grid(fmt);
        let before = planes.y.clone();
        deblock_mb_planes(&mut planes, &mbs, &slices, 3, 2, fmt);
        let changed = changed_rows(&planes.y, &before, planes.width, 16, 16);
        for row in [15usize, 16, 19, 20, 23, 24, 27, 28] {
            assert!(changed.contains(&row), "luma p0/q0 of edge rows {row} filtered (changed {changed:?})");
        }
    }

    /// I_PCM macroblocks index the tables with qP 0 (8.7.2.2); other macroblocks with the raw
    /// QPY — the effective QP in [`MbState`] minus QpBdOffsetY (12 at 10-bit).
    #[test]
    fn table_qp_is_raw_and_pcm_is_zero() {
        let mut inter = MbState { kind: MbKind::Inter, qp: 63, ..MbState::default() }; // effective of raw 51 at 10-bit
        assert_eq!(luma_table_qp(&inter, 12), 51);
        assert_eq!(luma_table_qp(&inter, 0), 63, "8-bit: effective == raw");
        inter.qpc = [3, 3]; // effective of raw QPC −9 at 10-bit
        assert_eq!(chroma_table_qp(&inter, 0, 12), -9, "raw QPC may be negative (8.5.8 NOTE 1)");
        let pcm = MbState { kind: MbKind::IPcm, ..MbState::default() };
        assert_eq!(luma_table_qp(&pcm, 12), 0, "I_PCM uses qP 0");
    }
}
