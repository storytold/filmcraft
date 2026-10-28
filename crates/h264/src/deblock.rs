//! Deblocking filter (8.7) for progressive frames, 4:2:0, 8-bit.

use crate::picture::{MbKind, MbState, Planes};
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
    let ids = ids8[b8];
    let count = (ids[0] != u32::MAX) as u8 + (ids[1] != u32::MAX) as u8;
    Motion { ids, mv: [st.mv[0][raster], st.mv[1][raster]], count }
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

#[cfg(test)]
fn clip3(lo: i32, hi: i32, v: i32) -> i32 {
    v.clamp(lo, hi)
}

/// Filter one set of eight samples p3 p2 p1 p0 | q0 q1 q2 q3 across an edge (8.7.2.3 / 8.7.2.4):
/// the per-line reference for [`filter_lanes`].
#[cfg(test)]
fn filter8(v: &mut [u8; 8], bs: u8, alpha: i32, beta: i32, tc0: i32, chroma: bool) {
    let p0 = v[3] as i32;
    let q0 = v[4] as i32;
    let p1 = v[2] as i32;
    let q1 = v[5] as i32;
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    if chroma {
        if bs < 4 {
            let tc = tc0 + 1;
            let delta = clip3(-tc, tc, (((q0 - p0) << 2) + (p1 - q1) + 4) >> 3);
            v[3] = (p0 + delta).clamp(0, 255) as u8;
            v[4] = (q0 - delta).clamp(0, 255) as u8;
        } else {
            v[3] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
            v[4] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
        }
        return;
    }
    let p2 = v[1] as i32;
    let q2 = v[6] as i32;
    let ap = (p2 - p0).abs();
    let aq = (q2 - q0).abs();
    if bs < 4 {
        let tc = tc0 + (ap < beta) as i32 + (aq < beta) as i32;
        let delta = clip3(-tc, tc, (((q0 - p0) << 2) + (p1 - q1) + 4) >> 3);
        v[3] = (p0 + delta).clamp(0, 255) as u8;
        v[4] = (q0 - delta).clamp(0, 255) as u8;
        if ap < beta {
            v[2] = (p1 + clip3(-tc0, tc0, (p2 + ((p0 + q0 + 1) >> 1) - (p1 << 1)) >> 1)) as u8;
        }
        if aq < beta {
            v[5] = (q1 + clip3(-tc0, tc0, (q2 + ((p0 + q0 + 1) >> 1) - (q1 << 1)) >> 1)) as u8;
        }
    } else {
        let strong = (p0 - q0).abs() < ((alpha >> 2) + 2);
        if ap < beta && strong {
            let p3 = v[0] as i32;
            v[3] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3) as u8;
            v[2] = ((p2 + p1 + p0 + q0 + 2) >> 2) as u8;
            v[1] = ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3) as u8;
        } else {
            v[3] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
        }
        if aq < beta && strong {
            let q3 = v[7] as i32;
            v[4] = ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3) as u8;
            v[5] = ((p0 + q0 + q1 + q2 + 2) >> 2) as u8;
            v[6] = ((2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3) as u8;
        } else {
            v[4] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
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

/// Filter one edge for `N` lines at once (lane `i` = line `i` across the edge). `s[k][i]` holds
/// sample p3 p2 p1 p0 q0 q1 q2 q3 (k = 0..8) of line `i`. Branch-free per lane, so the loops
/// vectorise; bit-exact with [`filter8`] applied to every line (8.7.2.3 / 8.7.2.4).
#[inline(always)]
fn filter_lanes<const N: usize>(s: &mut [[i16; N]; 8], ep: &EdgeParams, chroma: bool) {
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
fn filter_vertical<const N: usize>(pix: &mut [u8], stride: usize, x: usize, y: usize, ep: &EdgeParams, chroma: bool) {
    // Each line's eight samples as one little-endian word: byte k = sample k (p3 .. q3).
    let mut w = [0u64; N];
    for (i, w) in w.iter_mut().enumerate() {
        let o = (y + i) * stride + x - 4;
        let Some(&b) = pix.get(o..).and_then(<[u8]>::first_chunk::<8>) else {
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
fn filter_horizontal<const N: usize>(pix: &mut [u8], stride: usize, x: usize, y: usize, ep: &EdgeParams, chroma: bool) {
    let base = (y - 4) * stride + x;
    let mut s = [[0i16; N]; 8];
    for (k, row) in s.iter_mut().enumerate() {
        let Some(src) = pix.get(base + k * stride..).and_then(<[u8]>::first_chunk::<N>) else {
            return;
        };
        for i in 0..N {
            row[i] = src[i] as i16;
        }
    }
    filter_lanes(&mut s, ep, chroma);
    let (k0, k1) = if chroma { (3, 5) } else { (1, 7) };
    for (k, row) in s.iter().enumerate().take(k1).skip(k0) {
        let Some(dst) = pix.get_mut(base + k * stride..).and_then(<[u8]>::first_chunk_mut::<N>) else {
            return;
        };
        for i in 0..N {
            dst[i] = row[i] as u8;
        }
    }
}

/// Boundary strengths `[dir][edge][segment]`; dir 0 = vertical edges (x), 1 = horizontal edges (y).
type MbBs = [[[u8; 4]; 4]; 2];

/// Boundary strengths of an intra macroblock (8.7.2.1).
fn intra_mb_bs(t8: bool, left: Option<usize>, top: Option<usize>) -> MbBs {
    let mut bs = [[[0u8; 4]; 4]; 2];
    for (dir, neighbor) in [left, top].into_iter().enumerate() {
        if neighbor.is_some() {
            bs[dir][0] = [4; 4];
        }
        for e in 1..4 {
            if !(t8 && e % 2 == 1) {
                bs[dir][e] = [3; 4];
            }
        }
    }
    bs
}

/// Boundary strengths of the macroblock edge between neighbour `p` and the inter macroblock `q`.
fn inter_outer_edge_bs(pic: &PicState, p: &MbState, q: &MbState, qm: &[Motion; 16], dir: usize) -> [u8; 4] {
    if p.kind.is_intra() {
        return [4; 4];
    }
    let pids = ref_ids8(p, &pic.slices[p.slice_num as usize]);
    let mut out = [0u8; 4];
    for (k, o) in out.iter_mut().enumerate() {
        let (rq, rp) = if dir == 0 { (k * 4, k * 4 + 3) } else { (k, 12 + k) };
        *o = if (p.nz_mask >> rp) & 1 != 0 || (q.nz_mask >> rq) & 1 != 0 { 2 } else { motion_bs(&block_motion(p, &pids, rp), &qm[rq]) };
    }
    out
}

/// Boundary strengths of internal edge `e` (1..4) of the inter macroblock `q`.
fn inter_inner_edge_bs(q: &MbState, qm: &[Motion; 16], dir: usize, e: usize) -> [u8; 4] {
    let mut out = [0u8; 4];
    for (k, o) in out.iter_mut().enumerate() {
        let (rq, rp) = if dir == 0 { (k * 4 + e, k * 4 + e - 1) } else { (e * 4 + k, e * 4 + k - 4) };
        *o = if (q.nz_mask >> rp) & 1 != 0 || (q.nz_mask >> rq) & 1 != 0 { 2 } else { motion_bs(&qm[rp], &qm[rq]) };
    }
    out
}

/// Boundary strengths of an inter macroblock (8.7.2.1).
fn inter_mb_bs(pic: &PicState, q: &MbState, sq: &SliceInfo, left: Option<usize>, top: Option<usize>) -> MbBs {
    let mut bs = [[[0u8; 4]; 4]; 2];
    let t8 = q.transform_8x8;
    let qids = ref_ids8(q, sq);
    let q_uniform = (0..2).all(|l| {
        let r = q.ref_idx[l];
        r[1] == r[0] && r[2] == r[0] && r[3] == r[0] && q.mv[l].iter().all(|m| *m == q.mv[l][0])
    });
    let qm: [Motion; 16] = std::array::from_fn(|r| block_motion(q, &qids, r));
    for (dir, neighbor) in [left, top].into_iter().enumerate() {
        if let Some(n) = neighbor {
            bs[dir][0] = inter_outer_edge_bs(pic, &pic.mbs[n], q, &qm, dir);
        }
        if q_uniform && q.nz_mask == 0 {
            // one motion for the whole MB and no coefficients: all internal edges have bS 0
            continue;
        }
        for e in 1..4 {
            if !(t8 && e % 2 == 1) {
                bs[dir][e] = inter_inner_edge_bs(q, &qm, dir, e);
            }
        }
    }
    bs
}

/// Slice-level filter offsets (`FilterOffsetA` / `FilterOffsetB`).
#[derive(Clone, Copy)]
struct FilterOffsets {
    alpha: i32,
    beta: i32,
}

/// Edge filter parameters for neighbouring QPs `qpp` / `qpq` (8.7.2.2).
fn edge_params(qpp: i32, qpq: i32, b: [u8; 4], off: FilterOffsets) -> EdgeParams {
    let qpav = (qpp + qpq + 1) >> 1;
    let index_a = (qpav + off.alpha).clamp(0, 51) as usize;
    let index_b = (qpav + off.beta).clamp(0, 51) as usize;
    let tc = |b: u8| if (1..4).contains(&b) { TC0[index_a][b as usize - 1] as i32 } else { 0 };
    EdgeParams { bs: b, tc0: [tc(b[0]), tc(b[1]), tc(b[2]), tc(b[3])], alpha: ALPHA[index_a] as i32, beta: BETA[index_b] as i32 }
}

/// Everything the per-plane edge filters need to know about one macroblock.
struct MbEdges {
    bs: MbBs,
    offsets: FilterOffsets,
    mx: usize,
    my: usize,
    q_qp: i32,
    q_qpc: [u8; 2],
    /// (luma QP, chroma QPs) of the left and top neighbours.
    p_qp: [Option<(i32, [u8; 2])>; 2],
}

/// Filter the luma edges of one macroblock.
fn filter_luma_edges(planes: &mut Planes, m: &MbEdges) {
    let width = planes.width;
    for dir in 0..2 {
        for e in 0..4 {
            if m.bs[dir][e] == [0; 4] {
                continue;
            }
            let qpp = if e == 0 { m.p_qp[dir].map(|p| p.0).unwrap_or(m.q_qp) } else { m.q_qp };
            let ep = edge_params(qpp, m.q_qp, m.bs[dir][e], m.offsets);
            if ep.alpha == 0 || ep.beta == 0 {
                // filterSamplesFlag needs |p0 - q0| < alpha and |p1 - p0| < beta (8.7.2.2): no
                // sample of the edge changes (low QPs)
                continue;
            }
            if dir == 0 {
                filter_vertical::<16>(&mut planes.y, width, m.mx * 16 + e * 4, m.my * 16, &ep, false);
            } else {
                filter_horizontal::<16>(&mut planes.y, width, m.mx * 16, m.my * 16 + e * 4, &ep, false);
            }
        }
    }
}

/// Filter the chroma (4:2:0) edges of one macroblock: edges at chroma samples 0 and 4 use the bS of
/// luma edges 0 and 2.
fn filter_chroma_edges(planes: &mut Planes, m: &MbEdges) {
    let cwidth = planes.cwidth;
    for c in 0..2 {
        for dir in 0..2 {
            for ce in 0..2 {
                let e = ce * 2;
                if m.bs[dir][e] == [0; 4] {
                    continue;
                }
                let qpp = if e == 0 { m.p_qp[dir].map(|p| p.1[c]).unwrap_or(m.q_qpc[c]) } else { m.q_qpc[c] } as i32;
                let ep = edge_params(qpp, m.q_qpc[c] as i32, m.bs[dir][e], m.offsets);
                if ep.alpha == 0 || ep.beta == 0 {
                    continue;
                }
                let plane = if c == 0 { &mut planes.cb } else { &mut planes.cr };
                if dir == 0 {
                    filter_vertical::<8>(plane, cwidth, m.mx * 8 + ce * 4, m.my * 8, &ep, true);
                } else {
                    filter_horizontal::<8>(plane, cwidth, m.mx * 8, m.my * 8 + ce * 4, &ep, true);
                }
            }
        }
    }
}

/// Deblock one macroblock (macroblocks must be processed in raster order).
pub fn deblock_mb(pic: &mut PicState, addr: usize, mb_w: usize) {
    let q = &pic.mbs[addr];
    if q.slice_num == u32::MAX {
        return;
    }
    let sq = &pic.slices[q.slice_num as usize];
    if sq.disable_deblocking_filter_idc == 1 {
        return;
    }
    let (mx, my) = (addr % mb_w, addr / mb_w);
    let usable = |n: Option<usize>| -> Option<usize> {
        let n = n?;
        let st = &pic.mbs[n];
        if st.slice_num == u32::MAX || (sq.disable_deblocking_filter_idc == 2 && st.slice_num != q.slice_num) {
            return None;
        }
        Some(n)
    };
    let left = usable(if mx > 0 { Some(addr - 1) } else { None });
    let top = usable(if my > 0 { Some(addr - mb_w) } else { None });
    let bs = if q.kind.is_intra() { intra_mb_bs(q.transform_8x8, left, top) } else { inter_mb_bs(pic, q, sq, left, top) };
    let qp_of = |st: &MbState| if st.kind == MbKind::IPcm { 0 } else { st.qp as i32 };
    let neighbor_qp = |n: Option<usize>| n.map(|n| (qp_of(&pic.mbs[n]), pic.mbs[n].qpc));
    let edges = MbEdges {
        bs,
        offsets: FilterOffsets { alpha: sq.alpha_offset, beta: sq.beta_offset },
        mx,
        my,
        q_qp: qp_of(q),
        q_qpc: q.qpc,
        p_qp: [neighbor_qp(left), neighbor_qp(top)],
    };
    filter_luma_edges(&mut pic.planes, &edges);
    filter_chroma_edges(&mut pic.planes, &edges);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vectorised edge filter equals the per-line reference [`filter8`] on random edges
    /// (small differences across the edge so that every filter branch is taken).
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
            let bs: [u8; 4] = if strong { [4; 4] } else { std::array::from_fn(|_| (next() % 4) as u8) };
            let index_a = (next() % 52) as usize;
            let index_b = (next() % 52) as usize;
            let tc = |b: u8| if (1..4).contains(&b) { TC0[index_a][b as usize - 1] as i32 } else { 0 };
            let ep = EdgeParams { bs, tc0: bs.map(tc), alpha: ALPHA[index_a] as i32, beta: BETA[index_b] as i32 };
            let base = (next() % 256) as i32;
            let spread = 1 + (next() % 24) as i32;
            let mut lines = [[0u8; 8]; 16];
            for l in lines.iter_mut() {
                for v in l.iter_mut() {
                    *v = (base + (next() % (2 * spread as u64 + 1)) as i32 - spread).clamp(0, 255) as u8;
                }
            }
            let n = if chroma { 8 } else { 16 };
            let mut want = lines;
            for (i, l) in want.iter_mut().enumerate().take(n) {
                let k = i / (n / 4);
                if bs[k] != 0 {
                    filter8(l, bs[k], ep.alpha, ep.beta, ep.tc0[k], chroma);
                }
            }
            let got: Vec<[u8; 8]> = if chroma {
                let mut s = [[0i16; 8]; 8];
                for i in 0..8 {
                    for k in 0..8 {
                        s[k][i] = lines[i][k] as i16;
                    }
                }
                filter_lanes(&mut s, &ep, true);
                (0..8).map(|i| std::array::from_fn(|k| s[k][i] as u8)).collect()
            } else {
                let mut s = [[0i16; 16]; 8];
                for i in 0..16 {
                    for k in 0..8 {
                        s[k][i] = lines[i][k] as i16;
                    }
                }
                filter_lanes(&mut s, &ep, false);
                (0..16).map(|i| std::array::from_fn(|k| s[k][i] as u8)).collect()
            };
            assert_eq!(&got[..], &want[..n], "iteration {iter}: bs {bs:?} indexA {index_a} indexB {index_b}");
        }
    }
}

#[cfg(test)]
mod speed {
    use super::*;

    /// Rough timing of the edge filters (`cargo test --release -p filmcraft-h264 --lib edge_filter_speed -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn edge_filter_speed() {
        let (w, h) = (1024usize, 256usize);
        let mut pix: Vec<u8> = (0..w * h).map(|i| (128 + ((i * 7919) % 13) as i32 - 6) as u8).collect();
        let eps = [
            EdgeParams { bs: [4; 4], tc0: [0; 4], alpha: 40, beta: 9 },
            EdgeParams { bs: [3; 4], tc0: [2; 4], alpha: 40, beta: 9 },
            EdgeParams { bs: [2, 1, 0, 2], tc0: [1, 1, 0, 1], alpha: 40, beta: 9 },
        ];
        let t0 = std::time::Instant::now();
        let mut n = 0;
        for _ in 0..20 {
            for my in 1..h / 16 {
                for mx in 1..w / 16 {
                    for (j, ep) in eps.iter().enumerate() {
                        filter_horizontal::<16>(&mut pix, w, mx * 16, my * 16 + j * 4, ep, false);
                        n += 1;
                    }
                }
            }
        }
        let dt = t0.elapsed();
        let tv = std::time::Instant::now();
        for _ in 0..20 {
            for my in 1..h / 16 {
                for mx in 1..w / 16 {
                    for (j, ep) in eps.iter().enumerate() {
                        filter_vertical::<16>(&mut pix, w, mx * 16 + j * 4, my * 16, ep, false);
                    }
                }
            }
        }
        println!("vertical: {:.1} ns/edge", tv.elapsed().as_nanos() as f64 / n as f64);
        let t1 = std::time::Instant::now();
        for _ in 0..20 {
            for my in 1..h / 16 {
                for mx in 1..w / 16 {
                    for ep in eps.iter() {
                        for (vert, x, y) in [(true, mx * 16, my * 16), (false, mx * 16, my * 16)] {
                            for i in 0..16 {
                                let b = ep.bs[i / 4];
                                if b == 0 {
                                    continue;
                                }
                                let mut v = [0u8; 8];
                                for k in 0..8 {
                                    v[k] = if vert { pix[(y + i) * w + x - 4 + k] } else { pix[(y - 4 + k) * w + x + i] };
                                }
                                filter8(&mut v, b, ep.alpha, ep.beta, ep.tc0[i / 4], false);
                                for k in 1..7 {
                                    if vert {
                                        pix[(y + i) * w + x - 4 + k] = v[k];
                                    } else {
                                        pix[(y - 4 + k) * w + x + i] = v[k];
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        println!("scalar reference (both): {:.1} ns/edge", t1.elapsed().as_nanos() as f64 / (2 * n) as f64);
        println!(
            "horizontal: {n} luma edges in {dt:?}: {:.1} ns/edge (checksum {})",
            dt.as_nanos() as f64 / n as f64,
            pix.iter().map(|&v| v as u64).sum::<u64>()
        );
    }
}
