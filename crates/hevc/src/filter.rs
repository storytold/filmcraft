//! In-loop filters and row publication: deblocking (8.7.2) and sample adaptive offset (8.7.3), run
//! row by row behind the CTU decoding so finished CTB rows can be published to later pictures early.
//!
//! Row r is deblocked once rows 0..=r+1 are decoded (row r+1 still needs the unfiltered bottom samples
//! of row r for intra prediction); SAO of row r runs once row r+1 is deblocked; then row r is final.

use crate::picture::{ColMv, FrameRow, Sample};
use crate::slicedec::{E_PU_H, E_PU_V, E_TU_H, E_TU_V, F_CBF, F_CODED, F_INTRA, F_NOFILTER, MvField, PicState};
use crate::spec_tables::{BETA_TABLE, TC_TABLE};
use crate::tables::qpc;

/// Called after each decoded CTB of CTB row `ry`.
pub fn ctb_decoded<S: Sample>(pic: &mut PicState<S>, ry: usize) {
    pic.row_done[ry] += 1;
    while pic.rows_complete < pic.hctb && pic.row_done[pic.rows_complete] as usize == pic.wctb {
        pic.rows_complete += 1;
    }
    advance(pic, false);
}

/// Run the filters / publication as far as the decoded rows allow (`all`: end of picture).
pub fn advance<S: Sample>(pic: &mut PicState<S>, all: bool) {
    let h = pic.hctb;
    loop {
        let r = pic.rows_deblocked;
        if r < h && (all || pic.rows_complete >= (r + 2).min(h)) {
            deblock_row(pic, r);
            pic.rows_deblocked += 1;
            continue;
        }
        let r = pic.rows_published;
        if r < h && pic.rows_deblocked >= (r + 2).min(h) {
            publish_row(pic, r);
            pic.rows_published += 1;
            continue;
        }
        break;
    }
}

/// End of picture: filter and publish everything.
pub fn finish<S: Sample>(pic: &mut PicState<S>) {
    pic.rows_complete = pic.hctb;
    advance(pic, true);
}

#[inline]
fn tc_beta(q: i32, bs: i32, tc_off: i32, beta_off: i32, bd: u32) -> (i32, i32) {
    let qb = (q + beta_off).clamp(0, 51);
    let qt = (q + 2 * (bs - 1) + tc_off).clamp(0, 53);
    ((TC_TABLE[qt as usize] as i32) << (bd - 8), (BETA_TABLE[qb as usize] as i32) << (bd - 8))
}

/// Boundary strength (8.7.2.4) between 4x4 blocks p and q, in slices `ps` and `qs` (indices into
/// `pic.slices`, from [`edge_params`]).
fn boundary_strength<S: Sample>(pic: &PicState<S>, pi: usize, qi: usize, tu_edge: bool, ps: u32, qs: u32) -> i32 {
    let (p, q) = (&pic.blk[pi], &pic.blk[qi]);
    if (p.flags | q.flags) & F_INTRA != 0 {
        return 2;
    }
    if tu_edge && (p.flags | q.flags) & F_CBF != 0 {
        return 1;
    }
    let (mp, mq) = (&pic.mvf[pi], &pic.mvf[qi]);
    // Same motion in the same slice (same reference lists): same pictures, no motion difference.
    if mp == mq && ps == qs {
        return 0;
    }
    motion_bs(pic, ps, mp, qs, mq)
}

/// The picture referenced by list `l` of motion `f` in slice `s`.
fn ref_id<S: Sample>(pic: &PicState<S>, s: u32, f: &MvField, l: usize) -> u32 {
    if s == u32::MAX {
        return u32::MAX;
    }
    pic.slices.get(s as usize).and_then(|sl| sl.ref_ids[l].get(f.ref_idx[l] as usize)).copied().unwrap_or(u32::MAX - 1)
}

fn motion_bs<S: Sample>(pic: &PicState<S>, ps: u32, p: &MvField, qs: u32, q: &MvField) -> i32 {
    let np = p.pred(0) as u32 + p.pred(1) as u32;
    let nq = q.pred(0) as u32 + q.pred(1) as u32;
    if np != nq {
        return 1;
    }
    let far = |a: [i16; 2], b: [i16; 2]| (a[0] as i32 - b[0] as i32).abs() >= 4 || (a[1] as i32 - b[1] as i32).abs() >= 4;
    if np == 1 {
        let lp = if p.pred(0) { 0 } else { 1 };
        let lq = if q.pred(0) { 0 } else { 1 };
        if ref_id(pic, ps, p, lp) != ref_id(pic, qs, q, lq) {
            return 1;
        }
        return far(p.mv[lp], q.mv[lq]) as i32;
    }
    let (p0, p1) = (ref_id(pic, ps, p, 0), ref_id(pic, ps, p, 1));
    let (q0, q1) = (ref_id(pic, qs, q, 0), ref_id(pic, qs, q, 1));
    if !((p0 == q0 && p1 == q1) || (p0 == q1 && p1 == q0)) {
        return 1;
    }
    if p0 != p1 {
        if p0 == q0 { (far(p.mv[0], q.mv[0]) || far(p.mv[1], q.mv[1])) as i32 } else { (far(p.mv[0], q.mv[1]) || far(p.mv[1], q.mv[0])) as i32 }
    } else {
        ((far(p.mv[0], q.mv[0]) || far(p.mv[1], q.mv[1])) && (far(p.mv[0], q.mv[1]) || far(p.mv[1], q.mv[0]))) as i32
    }
}

/// Whether the edge between blocks p and q (q = current block, owning the edge) is filtered, and the
/// slice-level parameters (tc offset, beta offset) of q's slice, then the slices of p and q.
fn edge_params<S: Sample>(pic: &PicState<S>, px: usize, py: usize, qx: usize, qy: usize) -> Option<(i32, i32, u32, u32)> {
    let s = pic.log2_ctb;
    let pc = (py >> s) * pic.wctb + (px >> s);
    let qc = (qy >> s) * pic.wctb + (qx >> s);
    let qs = pic.ctb_slice[qc];
    let ps = pic.ctb_slice[pc];
    if qs == u32::MAX || ps == u32::MAX {
        return None;
    }
    let sq = &pic.slices[qs as usize];
    if sq.deblock_disabled {
        return None;
    }
    if pc != qc {
        if pic.ctb_addr[pc] != pic.ctb_addr[qc] && !sq.lf_across {
            return None;
        }
        if !pic.pps.loop_filter_across_tiles && pic.layout.tile_of_rs(pc as u32) != pic.layout.tile_of_rs(qc as u32) {
            return None;
        }
    }
    Some((sq.tc_offset, sq.beta_offset, ps, qs))
}

/// Deblock CTB row `r`: vertical edges of the row, then horizontal edges whose q side is in the row.
fn deblock_row<S: Sample>(pic: &mut PicState<S>, r: usize) {
    if !pic.deblocking_enabled_anywhere || pic.draft {
        return;
    }
    let ctb = 1usize << pic.log2_ctb;
    let y0 = r * ctb;
    let y1 = (y0 + ctb).min(pic.height);
    let (w, w4) = (pic.width, pic.w4);
    let (bd, bdc) = (pic.bd_y, pic.bd_c);
    let (cb_off, cr_off) = (pic.pps.cb_qp_offset, pic.pps.cr_qp_offset);
    // chroma geometry: edges on the 8x8 chroma sample grid (8.7.2, edgeSpacing), each segment
    // 4 chroma samples long, with bS taken from the luma segment at its first sample
    let (sx, sy) = pic.chroma.shifts();
    let is420 = sy == 1;
    // vertical edges
    let mut bs_v = vec![0i8; w4 * ((y1 - y0) / 4)];
    for by in y0 / 4..y1 / 4 {
        for bx in (2..w4).step_by(2) {
            let qi = by * w4 + bx;
            let e = pic.blk[qi].edges;
            if e & (E_TU_V | E_PU_V) == 0 || pic.blk[qi].flags & F_CODED == 0 {
                continue;
            }
            let (x, y) = (bx * 4, by * 4);
            let Some((tc_off, beta_off, ps, qs)) = edge_params(pic, x - 1, y, x, y) else { continue };
            let bs = boundary_strength(pic, qi - 1, qi, e & E_TU_V != 0, ps, qs);
            bs_v[(by - y0 / 4) * w4 + bx] = bs as i8;
            if bs == 0 {
                continue;
            }
            let (p, q) = (&pic.blk[qi - 1], &pic.blk[qi]);
            let qpl = (p.qp as i32 + q.qp as i32 + 1) >> 1;
            let (tc, beta) = tc_beta(qpl, bs, tc_off, beta_off, bd);
            let (nfp, nfq) = (p.flags & F_NOFILTER != 0, q.flags & F_NOFILTER != 0);
            filter_luma(&mut pic.planes[0], EdgeSeg { o: y * w + x, step: 1, along: w }, beta, EdgeFilter { tc, nfp, nfq, bd });
        }
    }
    // chroma vertical edges (8 chroma samples = 16 luma samples apart; a 4-line chroma segment
    // spans 4 << sy luma lines), bS == 2 only
    let cw = pic.cwidth;
    for by in (y0 / 4..y1 / 4).step_by(1 << sy) {
        for bx in (2 << sx..w4).step_by(2 << sx) {
            if bs_v[(by - y0 / 4) * w4 + bx] != 2 {
                continue;
            }
            let (x, y) = (bx * 4, by * 4);
            let qi = by * w4 + bx;
            let Some((tc_off, ..)) = edge_params(pic, x - 1, y, x, y) else { continue };
            let (p, q) = (pic.blk[qi - 1], pic.blk[qi]);
            let (nfp, nfq) = (p.flags & F_NOFILTER != 0, q.flags & F_NOFILTER != 0);
            for (c, off) in [(1, cb_off), (2, cr_off)] {
                let qpi = ((p.qp as i32 + q.qp as i32 + 1) >> 1) + off;
                let qt = (qpc(qpi, is420) + 2 + tc_off).clamp(0, 53);
                let tc = (TC_TABLE[qt as usize] as i32) << (bdc - 8);
                filter_chroma(&mut pic.planes[c], EdgeSeg { o: (y >> sy) * cw + (x >> sx), step: 1, along: cw }, EdgeFilter { tc, nfp, nfq, bd: bdc });
            }
        }
    }
    // horizontal edges with the q side in this row
    let mut bs_h = vec![0i8; w4 * ((y1 - y0) / 4)];
    for by in (y0 / 4..y1 / 4).filter(|b| b % 2 == 0 && *b > 0) {
        for bx in 0..w4 {
            let qi = by * w4 + bx;
            let e = pic.blk[qi].edges;
            if e & (E_TU_H | E_PU_H) == 0 || pic.blk[qi].flags & F_CODED == 0 {
                continue;
            }
            let (x, y) = (bx * 4, by * 4);
            let Some((tc_off, beta_off, ps, qs)) = edge_params(pic, x, y - 1, x, y) else { continue };
            let pi = qi - w4;
            let bs = boundary_strength(pic, pi, qi, e & E_TU_H != 0, ps, qs);
            bs_h[(by - y0 / 4) * w4 + bx] = bs as i8;
            if bs == 0 {
                continue;
            }
            let (p, q) = (&pic.blk[pi], &pic.blk[qi]);
            let qpl = (p.qp as i32 + q.qp as i32 + 1) >> 1;
            let (tc, beta) = tc_beta(qpl, bs, tc_off, beta_off, bd);
            let (nfp, nfq) = (p.flags & F_NOFILTER != 0, q.flags & F_NOFILTER != 0);
            filter_luma(&mut pic.planes[0], EdgeSeg { o: y * w + x, step: w, along: 1 }, beta, EdgeFilter { tc, nfp, nfq, bd });
        }
    }
    // chroma horizontal edges (8 chroma lines = 8 << sy luma lines apart; a 4-sample chroma
    // segment spans 4 << sx luma samples), bS == 2 only
    for by in (y0 / 4..y1 / 4).filter(|b| b % (2 << sy) == 0 && *b > 0) {
        for bx in (0..w4).step_by(1 << sx) {
            if bs_h[(by - y0 / 4) * w4 + bx] != 2 {
                continue;
            }
            let (x, y) = (bx * 4, by * 4);
            let qi = by * w4 + bx;
            let Some((tc_off, ..)) = edge_params(pic, x, y - 1, x, y) else { continue };
            let (p, q) = (pic.blk[qi - w4], pic.blk[qi]);
            let (nfp, nfq) = (p.flags & F_NOFILTER != 0, q.flags & F_NOFILTER != 0);
            for (c, off) in [(1, cb_off), (2, cr_off)] {
                let qpi = ((p.qp as i32 + q.qp as i32 + 1) >> 1) + off;
                let qt = (qpc(qpi, is420) + 2 + tc_off).clamp(0, 53);
                let tc = (TC_TABLE[qt as usize] as i32) << (bdc - 8);
                filter_chroma(&mut pic.planes[c], EdgeSeg { o: (y >> sy) * cw + (x >> sx), step: cw, along: 1 }, EdgeFilter { tc, nfp, nfq, bd: bdc });
            }
        }
    }
}

/// Where a 4-line edge segment lies in a plane: `o` = offset of q0 of the first line, `step` =
/// distance between p/q samples across the edge, `along` = distance between lines.
#[derive(Clone, Copy)]
struct EdgeSeg {
    o: usize,
    step: usize,
    along: usize,
}

/// How an edge segment is filtered: the clipping threshold `tc`, the no-filter flags of the p and
/// q sides and the bit depth.
#[derive(Clone, Copy)]
struct EdgeFilter {
    tc: i32,
    nfp: bool,
    nfq: bool,
    bd: u32,
}

/// Filter one 4-sample luma edge segment.
///
/// The 4 x 8 samples (p3..p0, q0..q3 of each line) are read once into an array, filtered there
/// and written back, a whole line at a time for vertical edges (`step` = 1) and a whole row of
/// the 4 lines at a time for horizontal ones (`along` = 1): no per-sample index arithmetic or
/// bounds checks in the filter itself.
fn filter_luma<S: Sample>(s: &mut [S], seg: EdgeSeg, beta: i32, ef: EdgeFilter) {
    let EdgeSeg { o, step, along } = seg;
    let EdgeFilter { tc, nfp, nfq, bd } = ef;
    // l[k][i]: line k, i = 0..4 -> p3, p2, p1, p0 and i = 4..8 -> q0, q1, q2, q3
    let mut l = [[0i32; 8]; 4];
    let Some(start) = o.checked_sub(4 * step) else { return };
    if step == 1 {
        for (k, line) in l.iter_mut().enumerate() {
            let Some(src) = s.get(start + k * along..).and_then(|v| v.first_chunk::<8>()) else { return };
            for (d, &v) in line.iter_mut().zip(src) {
                *d = v.to_i32();
            }
        }
    } else {
        for i in 0..8 {
            let Some(src) = s.get(start + i * step..).and_then(|v| v.first_chunk::<4>()) else { return };
            for (k, &v) in src.iter().enumerate() {
                l[k][i] = v.to_i32();
            }
        }
    }
    if !filter_luma_lines(&mut l, tc, beta, nfp, nfq, bd) {
        return;
    }
    if step == 1 {
        for (k, line) in l.iter().enumerate() {
            let Some(dst) = s.get_mut(start + k * along..).and_then(|v| v.first_chunk_mut::<8>()) else { return };
            for (d, &v) in dst.iter_mut().zip(line) {
                *d = S::from_i32(v);
            }
        }
    } else {
        for i in 1..7 {
            let Some(dst) = s.get_mut(start + i * step..).and_then(|v| v.first_chunk_mut::<4>()) else { return };
            for (k, d) in dst.iter_mut().enumerate() {
                *d = S::from_i32(l[k][i]);
            }
        }
    }
}

/// The luma edge filter (8.7.2.5.3 decisions, 8.7.2.5.7 filtering) on the lines of
/// [`filter_luma`]; false when the edge is left unfiltered (d >= beta).
fn filter_luma_lines(l: &mut [[i32; 8]; 4], tc: i32, beta: i32, nfp: bool, nfq: bool, bd: u32) -> bool {
    let px = |l: &[[i32; 8]; 4], k: usize, i: usize| l[k][3 - i];
    let qx = |l: &[[i32; 8]; 4], k: usize, i: usize| l[k][4 + i];
    let dp0 = (px(l, 0, 2) - 2 * px(l, 0, 1) + px(l, 0, 0)).abs();
    let dp3 = (px(l, 3, 2) - 2 * px(l, 3, 1) + px(l, 3, 0)).abs();
    let dq0 = (qx(l, 0, 2) - 2 * qx(l, 0, 1) + qx(l, 0, 0)).abs();
    let dq3 = (qx(l, 3, 2) - 2 * qx(l, 3, 1) + qx(l, 3, 0)).abs();
    let (dpq0, dpq3) = (dp0 + dq0, dp3 + dq3);
    let (dp, dq) = (dp0 + dp3, dq0 + dq3);
    let d = dpq0 + dpq3;
    if d >= beta {
        return false;
    }
    let dsam = |l: &[[i32; 8]; 4], k: usize, dpq: i32| -> bool {
        dpq < (beta >> 2)
            && (px(l, k, 3) - px(l, k, 0)).abs() + (qx(l, k, 0) - qx(l, k, 3)).abs() < (beta >> 3)
            && (px(l, k, 0) - qx(l, k, 0)).abs() < ((5 * tc + 1) >> 1)
    };
    let strong = dsam(l, 0, 2 * dpq0) && dsam(l, 3, 2 * dpq3);
    let dep = dp < ((beta + (beta >> 1)) >> 3);
    let deq = dq < ((beta + (beta >> 1)) >> 3);
    let max = (1i32 << bd) - 1;
    for line in l.iter_mut() {
        let [p3, p2, p1, p0, q0, q1, q2, q3] = *line;
        if strong {
            let t2 = 2 * tc;
            if !nfp {
                line[3] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3).clamp(p0 - t2, p0 + t2);
                line[2] = ((p2 + p1 + p0 + q0 + 2) >> 2).clamp(p1 - t2, p1 + t2);
                line[1] = ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3).clamp(p2 - t2, p2 + t2);
            }
            if !nfq {
                line[4] = ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3).clamp(q0 - t2, q0 + t2);
                line[5] = ((p0 + q0 + q1 + q2 + 2) >> 2).clamp(q1 - t2, q1 + t2);
                line[6] = ((p0 + q0 + q1 + 3 * q2 + 2 * q3 + 4) >> 3).clamp(q2 - t2, q2 + t2);
            }
        } else {
            let mut delta = (9 * (q0 - p0) - 3 * (q1 - p1) + 8) >> 4;
            if delta.abs() >= tc * 10 {
                continue;
            }
            delta = delta.clamp(-tc, tc);
            if !nfp {
                line[3] = (p0 + delta).clamp(0, max);
            }
            if !nfq {
                line[4] = (q0 - delta).clamp(0, max);
            }
            if dep && !nfp {
                let dp = ((((p2 + p0 + 1) >> 1) - p1 + delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                line[2] = (p1 + dp).clamp(0, max);
            }
            if deq && !nfq {
                let dq = ((((q2 + q0 + 1) >> 1) - q1 - delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                line[5] = (q1 + dq).clamp(0, max);
            }
        }
    }
    true
}

/// Filter one 4-sample chroma edge segment.
fn filter_chroma<S: Sample>(s: &mut [S], seg: EdgeSeg, ef: EdgeFilter) {
    let EdgeSeg { o, step, along } = seg;
    let EdgeFilter { tc, nfp, nfq, bd } = ef;
    let max = (1i32 << bd) - 1;
    for k in 0..4 {
        let base = o + k * along;
        let (p0, p1) = (s[base - step].to_i32(), s[base - 2 * step].to_i32());
        let (q0, q1) = (s[base].to_i32(), s[base + step].to_i32());
        let delta = ((((q0 - p0) << 2) + p1 - q1 + 4) >> 3).clamp(-tc, tc);
        if !nfp {
            s[base - step] = S::from_i32((p0 + delta).clamp(0, max));
        }
        if !nfq {
            s[base] = S::from_i32((q0 - delta).clamp(0, max));
        }
    }
}

/// SAO of CTB row `r` from the deblocked planes into new row buffers (luma, Cb, Cr).
fn sao_row<S: Sample>(pic: &PicState<S>, r: usize) -> [Vec<S>; 3] {
    let ctb = 1usize << pic.log2_ctb;
    let (sx, sy) = pic.chroma.shifts();
    let mut out: [Vec<S>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for c in 0..3 {
        // CTB size in this component's samples, horizontally and vertically
        let (w, h, csx, csy) = if c == 0 { (pic.width, pic.height, ctb, ctb) } else { (pic.cwidth, pic.cheight, ctb >> sx, ctb >> sy) };
        let y0 = r * csy;
        let y1 = (y0 + csy).min(h);
        out[c] = pic.planes[c][y0 * w..y1 * w].to_vec();
        for rx in (0..pic.wctb).filter(|_| !pic.draft) {
            let rs = r * pic.wctb + rx;
            let si = pic.ctb_slice[rs];
            if si == u32::MAX {
                continue;
            }
            let sl = &pic.slices[si as usize];
            if !(if c == 0 { sl.sao_luma } else { sl.sao_chroma }) {
                continue;
            }
            let p = pic.sao[rs];
            if p.type_idx[c] == 0 {
                continue;
            }
            let x0 = rx * csx;
            let x1 = (x0 + csx).min(w);
            sao_ctb(pic, &SaoBlock { c, rs, x0, x1, y0, y1, w, h }, &p, &mut out[c]);
        }
    }
    out
}

/// A CTB of plane `c` (CTB raster address `rs`) to apply SAO to: columns `x0..x1` and rows
/// `y0..y1` of a plane `w` x `h` samples.
#[derive(Clone, Copy)]
struct SaoBlock {
    c: usize,
    rs: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    w: usize,
    h: usize,
}

fn sao_ctb<S: Sample>(pic: &PicState<S>, blk: &SaoBlock, p: &crate::slicedec::SaoParams, dst: &mut [S]) {
    let SaoBlock { c, rs, x0, x1, y0, y1, w, h } = *blk;
    let bd = if c == 0 { pic.bd_y } else { pic.bd_c };
    let max = (1i32 << bd) - 1;
    // shifts from this component's sample positions to luma positions
    let (scx, scy) = if c == 0 { (0, 0) } else { pic.chroma.shifts() };
    let offs = p.offsets[c].map(|v| v as i32);
    let plane = &pic.planes[c];
    if p.type_idx[c] == 1 {
        let band = (p.band_pos[c] as i32, offs);
        for y in y0..y1 {
            sao_band_run(&plane[y * w + x0..y * w + x1], &mut dst[(y - y0) * w + x0..(y - y0) * w + x1], bd - 5, band, max);
        }
    } else {
        sao_edge_ctb(pic, c, rs, x0, x1, y0, y1, w, h, p, max, dst);
    }
    // Samples of CUs that must not be modified (transquant bypass, PCM without loop filter) get
    // their deblocked values back.
    if pic.pps.transquant_bypass || pic.sps.pcm_loop_filter_disabled {
        // one 4x4 luma block covers (4 >> scx) x (4 >> scy) samples of this component
        let (nx, ny) = (4 >> scx, 4 >> scy);
        for by in ((y0 << scy) >> 2)..(((y1 << scy) + 3) >> 2).min(pic.h4) {
            let ya = (by * 4) >> scy;
            for bx in ((x0 << scx) >> 2)..(((x1 << scx) + 3) >> 2).min(pic.w4) {
                if pic.blk[by * pic.w4 + bx].flags & F_NOFILTER == 0 {
                    continue;
                }
                let xa = (bx * 4) >> scx;
                let xb = (xa + nx).min(x1);
                for y in ya..(ya + ny).min(y1) {
                    dst[(y - y0) * w + xa..(y - y0) * w + xb].copy_from_slice(&plane[y * w + xa..y * w + xb]);
                }
            }
        }
    }
}

/// Edge offset of one CTB. Which of the 8 neighbouring CTBs may be used (inside the picture, not
/// in another slice / tile without cross-boundary filtering) is worked out once; each line is then
/// split into the left column, the inner run and the right column, and each part whose neighbours
/// are usable goes through [`sao_edge_run`].
#[allow(clippy::too_many_arguments)]
fn sao_edge_ctb<S: Sample>(
    pic: &PicState<S>,
    c: usize,
    rs: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    w: usize,
    h: usize,
    p: &crate::slicedec::SaoParams,
    max: i32,
    dst: &mut [S],
) {
    let offs = p.offsets[c].map(|v| v as i32);
    let (hp, vp): ([isize; 2], [isize; 2]) = match p.eo_class[c] {
        0 => ([-1, 1], [0, 0]),
        1 => ([0, 0], [-1, 1]),
        2 => ([-1, 1], [-1, 1]),
        _ => ([1, -1], [-1, 1]),
    };
    let cur_slice = pic.ctb_slice[rs];
    let lf_cur = pic.slices[cur_slice as usize].lf_across;
    let cur_ts = pic.layout.rs_to_ts[rs];
    let cross_ok = |nrs: usize| -> bool {
        if nrs == rs {
            return true;
        }
        let ns = pic.ctb_slice[nrs];
        if ns == u32::MAX {
            return false;
        }
        if pic.ctb_addr[nrs] != pic.ctb_addr[rs] {
            // different slice: the later one (in decoding order) decides
            let nts = pic.layout.rs_to_ts[nrs];
            if nts < cur_ts && !lf_cur {
                return false;
            }
            if cur_ts < nts && !pic.slices[ns as usize].lf_across {
                return false;
            }
        }
        if !pic.pps.loop_filter_across_tiles && pic.layout.tile_id[pic.layout.rs_to_ts[nrs] as usize] != pic.layout.tile_id[cur_ts as usize] {
            return false;
        }
        true
    };
    // avail[dy][dx]: the CTB at (rx + dx - 1, ry + dy - 1) may be used.
    let (rx, ry) = (rs % pic.wctb, rs / pic.wctb);
    let mut avail = [[false; 3]; 3];
    for (dy, row) in avail.iter_mut().enumerate() {
        for (dx, a) in row.iter_mut().enumerate() {
            let inside = (dx != 0 || x0 > 0) && (dx != 2 || x1 < w) && (dy != 0 || y0 > 0) && (dy != 2 || y1 < h);
            *a = inside && cross_ok((ry + dy - 1) * pic.wctb + rx + dx - 1);
        }
    }
    let edge_map = [1usize, 2, 0, 3, 4];
    // Offset per (2 + sum of neighbour signs); edgeIdx 0 adds nothing.
    let by_sum = edge_map.map(|e| if e == 0 { 0 } else { offs[e - 1] });
    let side = |v: isize, lo: usize, hi: usize| {
        if v < lo as isize {
            0
        } else if v >= hi as isize {
            2
        } else {
            1
        }
    };
    let mx = (hp[0] != 0) as usize;
    let plane = &pic.planes[c];
    for y in y0..y1 {
        let (ya, yb) = (y as isize + vp[0], y as isize + vp[1]);
        let (dya, dyb) = (side(ya, y0, y1), side(yb, y0, y1));
        for (xs, xe) in [(x0, x0 + mx), (x0 + mx, x1 - mx), (x1 - mx, x1)] {
            if xs >= xe {
                continue;
            }
            let (xa, xb) = (xs as isize + hp[0], xs as isize + hp[1]);
            let n = xe - xs;
            if !avail[dya][side(xa, x0, x1)] || !avail[dyb][side(xb, x0, x1)] {
                continue;
            }
            let cur = &plane[y * w + xs..][..n];
            let na = &plane[ya as usize * w + xa as usize..][..n];
            let nb = &plane[yb as usize * w + xb as usize..][..n];
            let out = &mut dst[(y - y0) * w + xs..][..n];
            sao_edge_run(cur, na, nb, out, &by_sum, max);
        }
    }
}

/// Lanes of the SAO loops (16-bit arithmetic: samples are at most 12 bits, offsets 7 bits).
const SAO_LANES: usize = 16;

/// Edge offset of a run of samples whose two neighbours (`na`, `nb`) are all usable: per lane
/// the sum of the two neighbour signs selects the offset (no table lookup), so the loop
/// vectorises; bit-exact with the per-sample form.
#[inline(always)]
fn sao_edge_run<S: Sample>(cur: &[S], na: &[S], nb: &[S], out: &mut [S], by_sum: &[i32; 5], max: i32) {
    let o = by_sum.map(|v| v as i16);
    let max = max as i16;
    let lane = |v: i16, a: i16, b: i16| -> S {
        let s = ((v > a) as i16 - (v < a) as i16) + ((v > b) as i16 - (v < b) as i16);
        let m = |k: i16| -((s == k) as i16);
        let off = (o[0] & m(-2)) | (o[1] & m(-1)) | (o[3] & m(1)) | (o[4] & m(2));
        S::from_i32((v + off).max(0).min(max) as i32)
    };
    let n = cur.len();
    let full = n - n % SAO_LANES;
    for i in (0..full).step_by(SAO_LANES) {
        let (Some(c), Some(a), Some(b), Some(d)) = (lanes(cur, i), lanes(na, i), lanes(nb, i), lanes_mut(out, i)) else {
            return;
        };
        for l in 0..SAO_LANES {
            d[l] = lane(c[l].to_i16(), a[l].to_i16(), b[l].to_i16());
        }
    }
    for i in full..n {
        out[i] = lane(cur[i].to_i16(), na[i].to_i16(), nb[i].to_i16());
    }
}

/// `SAO_LANES` samples from `i`, if all present.
#[inline(always)]
fn lanes<S>(s: &[S], i: usize) -> Option<&[S; SAO_LANES]> {
    s.get(i..)?.first_chunk()
}

#[inline(always)]
fn lanes_mut<S>(s: &mut [S], i: usize) -> Option<&mut [S; SAO_LANES]> {
    s.get_mut(i..)?.first_chunk_mut()
}

/// Band offset of a run of samples: the band index relative to `band_pos` selects one of the
/// four offsets per lane.
#[inline(always)]
fn sao_band_run<S: Sample>(src: &[S], out: &mut [S], shift: u32, (band_pos, offs): (i32, [i32; 4]), max: i32) {
    let o = offs.map(|v| v as i16);
    let (bp, max) = (band_pos as i16, max as i16);
    let lane = |v: i16| -> S {
        let k = ((v >> shift) - bp) & 31;
        let m = |j: i16| -((k == j) as i16);
        let off = (o[0] & m(0)) | (o[1] & m(1)) | (o[2] & m(2)) | (o[3] & m(3));
        S::from_i32((v + off).max(0).min(max) as i32)
    };
    let n = src.len();
    let full = n - n % SAO_LANES;
    for i in (0..full).step_by(SAO_LANES) {
        let (Some(c), Some(d)) = (lanes(src, i), lanes_mut(out, i)) else {
            return;
        };
        for l in 0..SAO_LANES {
            d[l] = lane(c[l].to_i16());
        }
    }
    for i in full..n {
        out[i] = lane(src[i].to_i16());
    }
}

/// Publish CTB row `r` (final samples + compressed motion) into the frame.
fn publish_row<S: Sample>(pic: &mut PicState<S>, r: usize) {
    let ctb = 1usize << pic.log2_ctb;
    let (y0, y1) = (r * ctb, ((r + 1) * ctb).min(pic.height));
    let w16 = pic.width.div_ceil(16);
    let rows16 = (y1 - y0).div_ceil(16);
    let mut col = vec![ColMv::default(); w16 * rows16];
    for j in 0..rows16 {
        for i in 0..w16 {
            let (x, y) = (i * 16, y0 + j * 16);
            let bi = (y >> 2) * pic.w4 + (x >> 2);
            let b = &pic.blk[bi];
            if b.flags & F_CODED == 0 || b.flags & F_INTRA != 0 {
                continue;
            }
            let f = pic.mvf[bi];
            let ctbi = (y >> pic.log2_ctb) * pic.wctb + (x >> pic.log2_ctb);
            let si = pic.ctb_slice[ctbi];
            if si == u32::MAX {
                continue;
            }
            let sl = &pic.slices[si as usize];
            let mut cm = ColMv { mv: f.mv, ref_poc: [0; 2], flags: 0 };
            for l in 0..2 {
                if f.pred(l) {
                    let ri = f.ref_idx[l] as usize;
                    if ri < sl.ref_pocs[l].len() {
                        cm.flags |= 1 << l;
                        cm.ref_poc[l] = sl.ref_pocs[l][ri];
                        if sl.ref_lt[l][ri] {
                            cm.flags |= 4 << l;
                        }
                    }
                }
            }
            col[j * w16 + i] = cm;
        }
    }
    let [oy, ocb, ocr] = sao_row(pic, r);
    let row = FrameRow { y: oy.into(), cb: ocb.into(), cr: ocr.into(), col: col.into() };
    pic.frame.publish(r, row);
}

#[cfg(test)]
mod luma_filter_tests {
    use super::*;

    /// The luma edge filter as it was before reading the samples into an array (indexing the
    /// picture sample by sample), as the reference.
    fn reference(s: &mut [u16], seg: EdgeSeg, beta: i32, ef: EdgeFilter) {
        let EdgeSeg { o, step, along } = seg;
        let EdgeFilter { tc, nfp, nfq, bd } = ef;
        let px = |s: &[u16], k: usize, i: usize| s[o + k * along - (i + 1) * step] as i32;
        let qx = |s: &[u16], k: usize, i: usize| s[o + k * along + i * step] as i32;
        let dp0 = (px(s, 0, 2) - 2 * px(s, 0, 1) + px(s, 0, 0)).abs();
        let dp3 = (px(s, 3, 2) - 2 * px(s, 3, 1) + px(s, 3, 0)).abs();
        let dq0 = (qx(s, 0, 2) - 2 * qx(s, 0, 1) + qx(s, 0, 0)).abs();
        let dq3 = (qx(s, 3, 2) - 2 * qx(s, 3, 1) + qx(s, 3, 0)).abs();
        let (dpq0, dpq3) = (dp0 + dq0, dp3 + dq3);
        let (dp, dq) = (dp0 + dp3, dq0 + dq3);
        let d = dpq0 + dpq3;
        if d >= beta {
            return;
        }
        let dsam = |s: &[u16], k: usize, dpq: i32| -> bool {
            dpq < (beta >> 2)
                && (px(s, k, 3) - px(s, k, 0)).abs() + (qx(s, k, 0) - qx(s, k, 3)).abs() < (beta >> 3)
                && (px(s, k, 0) - qx(s, k, 0)).abs() < ((5 * tc + 1) >> 1)
        };
        let strong = dsam(s, 0, 2 * dpq0) && dsam(s, 3, 2 * dpq3);
        let dep = dp < ((beta + (beta >> 1)) >> 3);
        let deq = dq < ((beta + (beta >> 1)) >> 3);
        let max = (1i32 << bd) - 1;
        for k in 0..4 {
            let base = o + k * along;
            let p = |i: usize| s[base - (i + 1) * step] as i32;
            let q = |i: usize| s[base + i * step] as i32;
            let (p0, p1, p2, p3) = (p(0), p(1), p(2), p(3));
            let (q0, q1, q2, q3) = (q(0), q(1), q(2), q(3));
            if strong {
                let t2 = 2 * tc;
                if !nfp {
                    s[base - step] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3).clamp(p0 - t2, p0 + t2) as u16;
                    s[base - 2 * step] = ((p2 + p1 + p0 + q0 + 2) >> 2).clamp(p1 - t2, p1 + t2) as u16;
                    s[base - 3 * step] = ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3).clamp(p2 - t2, p2 + t2) as u16;
                }
                if !nfq {
                    s[base] = ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3).clamp(q0 - t2, q0 + t2) as u16;
                    s[base + step] = ((p0 + q0 + q1 + q2 + 2) >> 2).clamp(q1 - t2, q1 + t2) as u16;
                    s[base + 2 * step] = ((p0 + q0 + q1 + 3 * q2 + 2 * q3 + 4) >> 3).clamp(q2 - t2, q2 + t2) as u16;
                }
            } else {
                let mut delta = (9 * (q0 - p0) - 3 * (q1 - p1) + 8) >> 4;
                if delta.abs() >= tc * 10 {
                    continue;
                }
                delta = delta.clamp(-tc, tc);
                if !nfp {
                    s[base - step] = (p0 + delta).clamp(0, max) as u16;
                }
                if !nfq {
                    s[base] = (q0 - delta).clamp(0, max) as u16;
                }
                if dep && !nfp {
                    let dp = ((((p2 + p0 + 1) >> 1) - p1 + delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                    s[base - 2 * step] = (p1 + dp).clamp(0, max) as u16;
                }
                if deq && !nfq {
                    let dq = ((((q2 + q0 + 1) >> 1) - q1 - delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                    s[base + step] = (q1 + dq).clamp(0, max) as u16;
                }
            }
        }
    }

    /// Same samples out of the gathered-block filter as out of the reference, for vertical and
    /// horizontal edges, strong and weak filtering, both bit depths and the no-filter flags.
    #[test]
    fn gathered_luma_filter_matches_the_per_sample_filter() {
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let w = 16usize;
        let mut filtered = 0;
        for case in 0..4000 {
            let bd = if case % 2 == 0 { 8 } else { 10 };
            let max = (1u64 << bd) - 1;
            // smooth blocks (strong filtering) to noisy ones (no filtering)
            let base = next() % (max + 1);
            let spread = [0u64, 2, 8, 64, max][case % 5];
            let plane: Vec<u16> = (0..w * w).map(|_| (base + next() % (spread + 1)).min(max) as u16).collect();
            let tc = (next() % 40) as i32;
            let beta = (next() % 120) as i32;
            let (nfp, nfq) = (next() % 4 == 0, next() % 4 == 0);
            for vertical in [true, false] {
                let (o, step, along) = if vertical { (4 * w + 8, 1, w) } else { (8 * w + 4, w, 1) };
                let (mut a, mut b) = (plane.clone(), plane.clone());
                let (seg, ef) = (EdgeSeg { o, step, along }, EdgeFilter { tc, nfp, nfq, bd });
                filter_luma(&mut a, seg, beta, ef);
                reference(&mut b, seg, beta, ef);
                assert_eq!(a, b, "case {case} vertical {vertical}");
                filtered += (a != plane) as usize;
            }
        }
        assert!(filtered > 1000, "only {filtered} edges filtered");
    }
}
