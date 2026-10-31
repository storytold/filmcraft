//! In-loop deblocking filter (§8.7) for frame-coded 4:2:0 pictures without MBAFF, 8 to 10 bits.
//!
//! Operates on a band of macroblock rows that begins at a slice boundary; the top edge of the band is
//! never filtered (slices are MB-row aligned and either there is one slice, or
//! `disable_deblocking_filter_idc = 2` is used).

use crate::mbinfo::{MbInfo, Mv};
use crate::pixel::{Depth, Pixel};
use crate::tables::{ALPHA, BETA, TC0, chroma_qp_prime};

pub struct DeblockParams {
    pub alpha_offset: i32,
    pub beta_offset: i32,
    pub chroma_qp_offset: i32,
    /// Picture identity for (list, ref_idx 0).
    pub ref_ids: [i32; 2],
    pub depth: Depth,
}

/// A mutable band of rows of a padded plane: sample (x, y) with y relative to the band top.
pub struct BandPlane<'a, P> {
    pub data: &'a mut [P],
    pub stride: usize,
    pub pad: usize,
}

impl<P> BandPlane<'_, P> {
    #[inline]
    fn idx(&self, x: usize, y: usize) -> usize {
        y * self.stride + self.pad + x
    }
}

#[inline]
fn nz8(m: &MbInfo, blk: usize) -> bool {
    if m.t8x8 {
        let (x, y) = ((blk & 3) >> 1, (blk >> 2) >> 1);
        let b = y * 8 + x * 2;
        m.nnz[b] | m.nnz[b + 1] | m.nnz[b + 4] | m.nnz[b + 5] != 0
    } else {
        m.nnz[blk] != 0
    }
}

#[inline]
fn mvdiff(a: Mv, b: Mv) -> bool {
    (a.x as i32 - b.x as i32).abs() >= 4 || (a.y as i32 - b.y as i32).abs() >= 4
}

fn bs_inter(p: &MbInfo, ip: usize, q: &MbInfo, iq: usize, ids: &[i32; 2]) -> u8 {
    if nz8(p, ip) || nz8(q, iq) {
        return 2;
    }
    let rp = [p.ref_at(0, ip), p.ref_at(1, ip)];
    let rq = [q.ref_at(0, iq), q.ref_at(1, iq)];
    // Only ref_idx 0 is ever used per list, so the list index identifies the picture.
    let pic = |r: i8, l: usize| if r >= 0 { ids[l] } else { -1 };
    let pp = [pic(rp[0], 0), pic(rp[1], 1)];
    let pq = [pic(rq[0], 0), pic(rq[1], 1)];
    let np = (pp[0] >= 0) as u8 + (pp[1] >= 0) as u8;
    let nq = (pq[0] >= 0) as u8 + (pq[1] >= 0) as u8;
    if np != nq {
        return 1;
    }
    if np == 1 {
        let (a, ma) = if pp[0] >= 0 { (pp[0], p.mv[0][ip]) } else { (pp[1], p.mv[1][ip]) };
        let (b, mb) = if pq[0] >= 0 { (pq[0], q.mv[0][iq]) } else { (pq[1], q.mv[1][iq]) };
        if a != b {
            return 1;
        }
        return mvdiff(ma, mb) as u8;
    }
    if np == 0 {
        return 0;
    }
    // two motion vectors each
    let (mp0, mp1, mq0, mq1) = (p.mv[0][ip], p.mv[1][ip], q.mv[0][iq], q.mv[1][iq]);
    let same_set = (pp[0] == pq[0] && pp[1] == pq[1]) || (pp[0] == pq[1] && pp[1] == pq[0]);
    if !same_set {
        return 1;
    }
    if pp[0] != pp[1] {
        // two different pictures: compare mvs referring to the same picture
        if pp[0] == pq[0] { (mvdiff(mp0, mq0) || mvdiff(mp1, mq1)) as u8 } else { (mvdiff(mp0, mq1) || mvdiff(mp1, mq0)) as u8 }
    } else {
        ((mvdiff(mp0, mq0) || mvdiff(mp1, mq1)) && (mvdiff(mp0, mq1) || mvdiff(mp1, mq0))) as u8
    }
}

#[inline]
fn bs_of(p: &MbInfo, ip: usize, q: &MbInfo, iq: usize, mb_edge: bool, ids: &[i32; 2]) -> u8 {
    if p.kind.is_intra() || q.kind.is_intra() {
        return if mb_edge { 4 } else { 3 };
    }
    bs_inter(p, ip, q, iq, ids)
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn filter_luma<P: Pixel>(d: &mut [P], q0i: usize, step: usize, bs: u8, alpha: i32, beta: i32, tc0: i32, dep: Depth) {
    let p0 = d[q0i - step].get();
    let q0 = d[q0i].get();
    let p1 = d[q0i - 2 * step].get();
    let q1 = d[q0i + step].get();
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    let p2 = d[q0i - 3 * step].get();
    let q2 = d[q0i + 2 * step].get();
    let ap = (p2 - p0).abs();
    let aq = (q2 - q0).abs();
    if bs < 4 {
        let tc = tc0 + (ap < beta) as i32 + (aq < beta) as i32;
        let delta = ((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
        d[q0i - step] = P::clip(p0 + delta, dep);
        d[q0i] = P::clip(q0 - delta, dep);
        if ap < beta {
            d[q0i - 2 * step] = P::of(p1 + ((p2 + ((p0 + q0 + 1) >> 1) - (p1 << 1)) >> 1).clamp(-tc0, tc0));
        }
        if aq < beta {
            d[q0i + step] = P::of(q1 + ((q2 + ((p0 + q0 + 1) >> 1) - (q1 << 1)) >> 1).clamp(-tc0, tc0));
        }
    } else {
        let p3 = d[q0i - 4 * step].get();
        let q3 = d[q0i + 3 * step].get();
        let strong = (p0 - q0).abs() < ((alpha >> 2) + 2);
        if ap < beta && strong {
            d[q0i - step] = P::of((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3);
            d[q0i - 2 * step] = P::of((p2 + p1 + p0 + q0 + 2) >> 2);
            d[q0i - 3 * step] = P::of((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3);
        } else {
            d[q0i - step] = P::of((2 * p1 + p0 + q1 + 2) >> 2);
        }
        if aq < beta && strong {
            d[q0i] = P::of((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3);
            d[q0i + step] = P::of((p0 + q0 + q1 + q2 + 2) >> 2);
            d[q0i + 2 * step] = P::of((2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3);
        } else {
            d[q0i] = P::of((2 * q1 + q0 + p1 + 2) >> 2);
        }
    }
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn filter_chroma<P: Pixel>(d: &mut [P], q0i: usize, step: usize, bs: u8, alpha: i32, beta: i32, tc0: i32, dep: Depth) {
    let p0 = d[q0i - step].get();
    let q0 = d[q0i].get();
    let p1 = d[q0i - 2 * step].get();
    let q1 = d[q0i + step].get();
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    if bs < 4 {
        let tc = tc0 + 1;
        let delta = ((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
        d[q0i - step] = P::clip(p0 + delta, dep);
        d[q0i] = P::clip(q0 - delta, dep);
    } else {
        d[q0i - step] = P::of((2 * p1 + p0 + q1 + 2) >> 2);
        d[q0i] = P::of((2 * q1 + q0 + p1 + 2) >> 2);
    }
}

struct EdgeQ {
    alpha: i32,
    beta: i32,
    idx_a: usize,
    /// tC0 scale, 1 << (BitDepth - 8) (§8.7.2.3).
    tc_scale: i32,
}

impl EdgeQ {
    #[inline]
    fn tc0(&self, bs: u8) -> i32 {
        if bs < 4 { TC0[self.idx_a][bs as usize - 1] as i32 * self.tc_scale } else { 0 }
    }
}

/// Edge thresholds from the unprimed QPs (QPY, or QPC for chroma) of the two macroblocks; α and β are
/// scaled by 1 << (BitDepth - 8) (§8.7.2.2).
fn edge_q(qp_p: i32, qp_q: i32, dp: &DeblockParams) -> EdgeQ {
    let qav = (qp_p + qp_q + 1) >> 1;
    let ia = (qav + dp.alpha_offset).clamp(0, 51) as usize;
    let ib = (qav + dp.beta_offset).clamp(0, 51) as usize;
    let sh = dp.depth.shift;
    EdgeQ { alpha: (ALPHA[ia] as i32) << sh, beta: (BETA[ib] as i32) << sh, idx_a: ia, tc_scale: 1 << sh }
}

/// Deblock `rows` macroblock rows of a band. `mbs` holds the band's MbInfo (row-major, `mbw` wide).
pub fn deblock_band<'a, P: Pixel>(
    y: &mut BandPlane<'a, P>,
    u: &mut BandPlane<'a, P>,
    v: &mut BandPlane<'a, P>,
    mbs: &[MbInfo],
    mbw: usize,
    rows: usize,
    dp: &DeblockParams,
) {
    let ids = &dp.ref_ids;
    let off = dp.depth.qp_offset;
    for my in 0..rows {
        for mx in 0..mbw {
            let q = &mbs[my * mbw + mx];
            // MbInfo::qp is QP'Y; the filter works on QPY and QPC.
            let yqp = |m: &MbInfo| m.qp as i32 - off;
            let cqp = |m: &MbInfo| chroma_qp_prime(m.qp as i32, dp.chroma_qp_offset, off) as i32 - off;
            // ---- vertical edges (dir 0) then horizontal (dir 1)
            for dir in 0..2 {
                let mut bs_all = [[0u8; 4]; 4];
                for e in 0..4 {
                    if e == 0 && ((dir == 0 && mx == 0) || (dir == 1 && my == 0)) {
                        continue;
                    }
                    if e & 1 == 1 && q.t8x8 {
                        continue;
                    }
                    let p_mb = if e == 0 { if dir == 0 { &mbs[my * mbw + mx - 1] } else { &mbs[(my - 1) * mbw + mx] } } else { q };
                    let mut bs = [0u8; 4];
                    for s in 0..4 {
                        let (iq, ip) = if dir == 0 {
                            let iq = s * 4 + e;
                            (iq, if e == 0 { s * 4 + 3 } else { iq - 1 })
                        } else {
                            let iq = e * 4 + s;
                            (iq, if e == 0 { 12 + s } else { iq - 4 })
                        };
                        bs[s] = bs_of(p_mb, ip, q, iq, e == 0, ids);
                    }
                    bs_all[e] = bs;
                    if bs == [0; 4] {
                        continue;
                    }
                    let eq = edge_q(yqp(p_mb), yqp(q), dp);
                    if eq.alpha == 0 || eq.beta == 0 {
                        continue;
                    }
                    for k in 0..16 {
                        let b = bs[k >> 2];
                        if b == 0 {
                            continue;
                        }
                        let (qi, step) = if dir == 0 { (y.idx(mx * 16 + e * 4, my * 16 + k), 1) } else { (y.idx(mx * 16 + k, my * 16 + e * 4), y.stride) };
                        filter_luma(y.data, qi, step, b, eq.alpha, eq.beta, eq.tc0(b), dp.depth);
                    }
                }
                // chroma edges 0 and 2 (chroma sample offsets 0 and 4)
                for e in [0usize, 2] {
                    if e == 0 && ((dir == 0 && mx == 0) || (dir == 1 && my == 0)) {
                        continue;
                    }
                    let bs = bs_all[e];
                    if bs == [0; 4] {
                        continue;
                    }
                    let p_mb = if e == 0 { if dir == 0 { &mbs[my * mbw + mx - 1] } else { &mbs[(my - 1) * mbw + mx] } } else { q };
                    let eq = edge_q(cqp(p_mb), cqp(q), dp);
                    if eq.alpha == 0 || eq.beta == 0 {
                        continue;
                    }
                    for plane in [&mut *u, &mut *v] {
                        for k in 0..8 {
                            let b = bs[k >> 1];
                            if b == 0 {
                                continue;
                            }
                            let (qi, step) =
                                if dir == 0 { (plane.idx(mx * 8 + e * 2, my * 8 + k), 1) } else { (plane.idx(mx * 8 + k, my * 8 + e * 2), plane.stride) };
                            filter_chroma(plane.data, qi, step, b, eq.alpha, eq.beta, eq.tc0(b), dp.depth);
                        }
                    }
                }
            }
        }
    }
}
