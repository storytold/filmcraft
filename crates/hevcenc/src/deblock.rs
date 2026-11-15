//! Normative HEVC deblocking filter (spec §8.7.2) for the encoder reconstruction loop.
//! Sample-exact with the decoder-side filter: same beta/tC tables, strong/weak/none decisions, clips.

use crate::Error;
use crate::picture::Plane;
use crate::tables::{BETA_TABLE, TC_TABLE, qpc};

/// Per-8x8-block coding decisions over the picture grid, filled by the slice coder.
/// `mv` holds `(mvx_qpel, mvy_qpel, ref_idx)` of the PU covering the block's top-left (`-1` when intra).
#[derive(Clone, Debug)]
pub struct DeblockMaps {
    pub blocks_w: usize,
    pub blocks_h: usize,
    pub intra: Vec<bool>,
    pub cbf: Vec<bool>,
    pub mv: Vec<(i32, i32, i32)>,
    pub tu_edge_v: Vec<bool>,
    pub tu_edge_h: Vec<bool>,
    pub pu_edge_v: Vec<bool>,
    pub pu_edge_h: Vec<bool>,
}

/// Deblock the reconstructed picture in place: vertical edges, then horizontal (spec §8.7.2.1/8.7.2.5).
/// Caller runs `extend_edges` first; picture-boundary edges are never filtered. Length/size mismatch errors.
pub fn deblock_picture(y: &mut Plane, u: &mut Plane, v: &mut Plane, qp_y: &[u8], maps: &DeblockMaps) -> Result<(), Error> {
    let n = maps.blocks_w.checked_mul(maps.blocks_h).ok_or_else(|| Error::InvalidConfig("deblock grid overflows".to_string()))?;
    let lens =
        [maps.intra.len(), maps.cbf.len(), maps.mv.len(), maps.tu_edge_v.len(), maps.tu_edge_h.len(), maps.pu_edge_v.len(), maps.pu_edge_h.len(), qp_y.len()];
    if lens.iter().any(|&l| l != n) {
        return Err(Error::InvalidConfig(format!("deblock maps hold {lens:?} entries for a {}x{} grid ({n} blocks)", maps.blocks_w, maps.blocks_h)));
    }
    let lw = maps.blocks_w.checked_mul(8).ok_or_else(|| Error::InvalidConfig("deblock luma width overflows".to_string()))?;
    let lh = maps.blocks_h.checked_mul(8).ok_or_else(|| Error::InvalidConfig("deblock luma height overflows".to_string()))?;
    let cw = maps.blocks_w.checked_mul(4).ok_or_else(|| Error::InvalidConfig("deblock chroma width overflows".to_string()))?;
    let ch = maps.blocks_h.checked_mul(4).ok_or_else(|| Error::InvalidConfig("deblock chroma height overflows".to_string()))?;
    check_plane(y, lw, lh, "luma")?;
    check_plane(u, cw, ch, "Cb")?;
    check_plane(v, cw, ch, "Cr")?;
    let mut bs = vec![0u8; n]; // one edge's strength per 8x8 block, reused for the chroma pass and both directions
    // Vertical edges: block column bx owns its left edge; bx = 0 is the picture boundary (never filtered).
    for by in 0..maps.blocks_h {
        for bx in 1..maps.blocks_w {
            let qi = by * maps.blocks_w + bx; // in range: bx < blocks_w, by < blocks_h, all maps len n
            let pi = qi - 1;
            let b = boundary_strength(maps, pi, qi, maps.tu_edge_v[qi], maps.pu_edge_v[qi]);
            bs[qi] = b;
            if b == 0 {
                continue;
            }
            let (tc, beta) = luma_thresholds(qp_y[pi], qp_y[qi], b);
            let (x, y0) = (bx * 8, by * 8);
            let stride = y.stride;
            for s in 0..2 {
                let o = y.idx(x, y0 + s * 4);
                filter_luma_edge(&mut y.data, o, 1, stride, tc, beta);
            }
        }
    }
    // Chroma verticals: bS == 2 only, and only where the edge sits on the 16-luma grid (bx even, §8.7.2.5.1).
    for by in 0..maps.blocks_h {
        for bx in (2..maps.blocks_w).step_by(2) {
            let qi = by * maps.blocks_w + bx;
            if bs[qi] != 2 {
                continue;
            }
            let tc = chroma_tc(qp_y[qi - 1], qp_y[qi]);
            let (cx, cy) = (bx * 4, by * 4);
            let (ou, su) = (u.idx(cx, cy), u.stride);
            filter_chroma_edge(&mut u.data, ou, 1, su, tc);
            let (ov, sv) = (v.idx(cx, cy), v.stride);
            filter_chroma_edge(&mut v.data, ov, 1, sv, tc);
        }
    }
    // Horizontal edges: block row by owns its top edge; by = 0 is the picture boundary (never filtered).
    for by in 1..maps.blocks_h {
        for bx in 0..maps.blocks_w {
            let qi = by * maps.blocks_w + bx;
            let pi = qi - maps.blocks_w;
            let b = boundary_strength(maps, pi, qi, maps.tu_edge_h[qi], maps.pu_edge_h[qi]);
            bs[qi] = b;
            if b == 0 {
                continue;
            }
            let (tc, beta) = luma_thresholds(qp_y[pi], qp_y[qi], b);
            let (x0, yy) = (bx * 8, by * 8);
            let stride = y.stride;
            for s in 0..2 {
                let o = y.idx(x0 + s * 4, yy);
                filter_luma_edge(&mut y.data, o, stride, 1, tc, beta);
            }
        }
    }
    // Chroma horizontals: bS == 2 only, on the 16-luma grid (by even, §8.7.2.5.2).
    for by in (2..maps.blocks_h).step_by(2) {
        for bx in 0..maps.blocks_w {
            let qi = by * maps.blocks_w + bx;
            if bs[qi] != 2 {
                continue;
            }
            let tc = chroma_tc(qp_y[qi - maps.blocks_w], qp_y[qi]);
            let (cx0, cy) = (bx * 4, by * 4);
            let (ou, su) = (u.idx(cx0, cy), u.stride);
            filter_chroma_edge(&mut u.data, ou, su, 1, tc);
            let (ov, sv) = (v.idx(cx0, cy), v.stride);
            filter_chroma_edge(&mut v.data, ov, sv, 1, tc);
        }
    }
    Ok(())
}

/// Visible size, stride and backing store must agree before the hot loops index directly (hostile sizes rejected).
fn check_plane(p: &Plane, w: usize, h: usize, what: &str) -> Result<(), Error> {
    if p.w != w || p.h != h {
        return Err(Error::InvalidConfig(format!("deblock {what} plane is {}x{}, expected {w}x{h}", p.w, p.h)));
    }
    if p.stride < p.w {
        return Err(Error::InvalidConfig(format!("deblock {what} stride {} below width {}", p.stride, p.w)));
    }
    let pad = (p.stride - p.w) / 2;
    let rows =
        p.h.checked_add(pad.checked_mul(2).ok_or_else(|| Error::InvalidConfig(format!("deblock {what} padding overflows")))?)
            .ok_or_else(|| Error::InvalidConfig(format!("deblock {what} padded height overflows")))?;
    let need = p.stride.checked_mul(rows).ok_or_else(|| Error::InvalidConfig(format!("deblock {what} store size overflows")))?;
    if p.data.len() < need {
        return Err(Error::InvalidConfig(format!("deblock {what} store holds {}, needs {need}", p.data.len())));
    }
    Ok(())
}

/// Boundary strength of the left/top edge of 8x8 block `qi` against neighbour `pi` (§8.7.2.4 on our 8x8 grid).
#[inline]
fn boundary_strength(maps: &DeblockMaps, pi: usize, qi: usize, tu_edge: bool, pu_edge: bool) -> u8 {
    if !tu_edge && !pu_edge {
        return 0; // interior to every TU and PU: the spec sets no edge flag, so bS = 0
    }
    if maps.intra[pi] || maps.intra[qi] {
        return 2; // either side intra-predicted: strongest filtering
    }
    if tu_edge && (maps.cbf[pi] || maps.cbf[qi]) {
        return 1; // coded residual straddles a TU edge
    }
    if pu_edge {
        // Single-list phase-1 motion: different pictures, or >= 1 pel (4 qpel) apart, filters.
        let (px, py, pr) = maps.mv[pi];
        let (qx, qy, qr) = maps.mv[qi];
        if pr != qr || px.abs_diff(qx) >= 4 || py.abs_diff(qy) >= 4 {
            return 1;
        }
    }
    0
}

/// Luma beta/tC from the mean QP of the two sides (§8-347..351; zero slice offsets and 8-bit hard-coded).
#[inline]
fn luma_thresholds(qp_p: u8, qp_q: u8, bs: u8) -> (i32, i32) {
    let qpl = (u16::from(qp_p) + u16::from(qp_q) + 1) >> 1; // mean QP; min() below keeps hostile QPs in table range
    (i32::from(TC_TABLE[(qpl + 2 * (u16::from(bs) - 1)).min(53) as usize]), i32::from(BETA_TABLE[qpl.min(51) as usize]))
}

/// Chroma tC from the mean luma QP mapped through `qpc` (§8-382..384; zero picture/slice offsets, 8-bit).
#[inline]
fn chroma_tc(qp_p: u8, qp_q: u8) -> i32 {
    let qpl = ((u16::from(qp_p) + u16::from(qp_q) + 1) >> 1).min(51) as u8;
    i32::from(TC_TABLE[(u16::from(qpc(qpl)) + 2).min(53) as usize]) // callers only filter chroma at bS == 2
}

/// Filter one 4-line luma segment; `o` is the q0 sample of the first line, `step` crosses the edge, `along` walks lines.
fn filter_luma_edge(s: &mut [u8], o: usize, step: usize, along: usize, tc: i32, beta: i32) {
    let mut l = [[0i32; 8]; 4]; // lines of p3..p0, q0..q3 gathered once, so the filter does no index math
    let Some(start) = o.checked_sub(4 * step) else { return }; // short store: leave the edge alone, never panic
    if step == 1 {
        for (k, line) in l.iter_mut().enumerate() {
            let Some(src) = start.checked_add(k * along).and_then(|off| s.get(off..)).and_then(|v| v.first_chunk::<8>()) else { return };
            for (d, &v) in line.iter_mut().zip(src.iter()) {
                *d = i32::from(v);
            }
        }
    } else {
        for i in 0..8 {
            let Some(src) = s.get(start + i * step..).and_then(|v| v.first_chunk::<4>()) else { return };
            for (k, &v) in src.iter().enumerate() {
                l[k][i] = i32::from(v);
            }
        }
    }
    if !filter_luma_seg(&mut l, tc, beta) {
        return; // d >= beta: a real content edge, not a block artifact
    }
    if step == 1 {
        for (k, line) in l.iter().enumerate() {
            let Some(dst) = s.get_mut(start + k * along..).and_then(|v| v.first_chunk_mut::<8>()) else { return };
            for (d, &v) in dst.iter_mut().zip(line.iter()) {
                *d = v as u8; // every tap averages in-range samples, so it stays in 0..=255 by construction
            }
        }
    } else {
        for i in 1..7 {
            // p3/q3 are never written: strong filtering touches p2..q2 at most
            let Some(dst) = s.get_mut(start + i * step..).and_then(|v| v.first_chunk_mut::<4>()) else { return };
            for (k, d) in dst.iter_mut().enumerate() {
                *d = l[k][i] as u8;
            }
        }
    }
}

/// Luma strong/weak/none decision (§8.7.2.5.3/8.7.2.5.6) and sample update (§8.7.2.5.7); false filters nothing.
fn filter_luma_seg(l: &mut [[i32; 8]; 4], tc: i32, beta: i32) -> bool {
    let px = |l: &[[i32; 8]; 4], k: usize, i: usize| l[k][3 - i]; // p_i of line k
    let qx = |l: &[[i32; 8]; 4], k: usize, i: usize| l[k][4 + i]; // q_i of line k
    let dp0 = (px(l, 0, 2) - 2 * px(l, 0, 1) + px(l, 0, 0)).abs();
    let dp3 = (px(l, 3, 2) - 2 * px(l, 3, 1) + px(l, 3, 0)).abs();
    let dq0 = (qx(l, 0, 2) - 2 * qx(l, 0, 1) + qx(l, 0, 0)).abs();
    let dq3 = (qx(l, 3, 2) - 2 * qx(l, 3, 1) + qx(l, 3, 0)).abs();
    let (dpq0, dpq3) = (dp0 + dq0, dp3 + dq3);
    let (dp, dq) = (dp0 + dp3, dq0 + dq3);
    if dpq0 + dpq3 >= beta {
        return false;
    }
    let smooth = |l: &[[i32; 8]; 4], k: usize, dpq: i32| {
        dpq < (beta >> 2)
            && (px(l, k, 3) - px(l, k, 0)).abs() + (qx(l, k, 0) - qx(l, k, 3)).abs() < (beta >> 3)
            && (px(l, k, 0) - qx(l, k, 0)).abs() < ((5 * tc + 1) >> 1)
    };
    let strong = smooth(l, 0, 2 * dpq0) && smooth(l, 3, 2 * dpq3); // both outer lines smooth: filter three deep
    let dep = dp < ((beta + (beta >> 1)) >> 3); // p-side flat enough to touch p1
    let deq = dq < ((beta + (beta >> 1)) >> 3); // q-side flat enough to touch q1
    for line in l.iter_mut() {
        let [p3, p2, p1, p0, q0, q1, q2, q3] = *line; // originals: every tap below reads pre-filter values
        if strong {
            let t2 = 2 * tc;
            line[3] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3).clamp(p0 - t2, p0 + t2); // (8-389)
            line[2] = ((p2 + p1 + p0 + q0 + 2) >> 2).clamp(p1 - t2, p1 + t2); // (8-390)
            line[1] = ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3).clamp(p2 - t2, p2 + t2); // (8-391)
            line[4] = ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3).clamp(q0 - t2, q0 + t2); // (8-392)
            line[5] = ((p0 + q0 + q1 + q2 + 2) >> 2).clamp(q1 - t2, q1 + t2); // (8-393)
            line[6] = ((p0 + q0 + q1 + 3 * q2 + 2 * q3 + 4) >> 3).clamp(q2 - t2, q2 + t2); // (8-394)
        } else {
            let mut delta = (9 * (q0 - p0) - 3 * (q1 - p1) + 8) >> 4; // (8-395)
            if delta.abs() >= tc * 10 {
                continue; // large correction wanted: the edge is real content, skip this line
            }
            delta = delta.clamp(-tc, tc); // (8-396)
            line[3] = (p0 + delta).clamp(0, 255); // (8-397), 8-bit Clip1Y
            line[4] = (q0 - delta).clamp(0, 255); // (8-398)
            if dep {
                line[2] = (p1 + ((((p2 + p0 + 1) >> 1) - p1 + delta) >> 1).clamp(-(tc >> 1), tc >> 1)).clamp(0, 255); // (8-399/400)
            }
            if deq {
                line[5] = (q1 + ((((q2 + q0 + 1) >> 1) - q1 - delta) >> 1).clamp(-(tc >> 1), tc >> 1)).clamp(0, 255); // (8-401/402)
            }
        }
    }
    true
}

/// Filter one 4-sample chroma segment (§8.7.2.5.8); chroma has no activity check, only the tC clip.
fn filter_chroma_edge(s: &mut [u8], o: usize, step: usize, along: usize, tc: i32) {
    for k in 0..4 {
        let Some(base) = o.checked_add(k * along) else { return }; // short store: leave the rest, never panic
        let (Some(p0i), Some(p1i), Some(q1i)) = (base.checked_sub(step), base.checked_sub(2 * step), base.checked_add(step)) else { return };
        let (Some(&p1b), Some(&p0b), Some(&q0b), Some(&q1b)) = (s.get(p1i), s.get(p0i), s.get(base), s.get(q1i)) else { return };
        let (p1, p0, q0, q1) = (i32::from(p1b), i32::from(p0b), i32::from(q0b), i32::from(q1b));
        let delta = ((((q0 - p0) << 2) + p1 - q1 + 4) >> 3).clamp(-tc, tc); // (8-403)
        if let Some(p) = s.get_mut(p0i) {
            *p = (p0 + delta).clamp(0, 255) as u8; // (8-404), 8-bit Clip1C
        }
        if let Some(q) = s.get_mut(base) {
            *q = (q0 - delta).clamp(0, 255) as u8; // (8-405)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picture::Plane;

    fn plane(w: usize, h: usize) -> Plane {
        Plane::new(w, h, 8)
    }

    fn blank_maps(blocks_w: usize, blocks_h: usize) -> DeblockMaps {
        let n = blocks_w * blocks_h;
        DeblockMaps {
            blocks_w,
            blocks_h,
            intra: vec![false; n],
            cbf: vec![false; n],
            mv: vec![(0, 0, 0); n],
            tu_edge_v: vec![false; n],
            tu_edge_h: vec![false; n],
            pu_edge_v: vec![false; n],
            pu_edge_h: vec![false; n],
        }
    }

    fn fill(y: &mut Plane, f: impl Fn(usize, usize) -> u8) {
        for yy in 0..y.h {
            for xx in 0..y.w {
                let o = y.idx(xx, yy);
                y.data[o] = f(xx, yy);
            }
        }
    }

    fn at(y: &Plane, x: usize, yy: usize) -> u8 {
        y.data[y.idx(x, yy)]
    }

    fn row(y: &Plane, yy: usize) -> Vec<u8> {
        (0..y.w).map(|x| at(y, x, yy)).collect()
    }

    // Independent scalar re-derivations of §8.7.2.5.7/8.7.2.5.8, written straight from the spec
    // equations (mirrors the decoder-side logic in hevc/src/filter.rs without sharing code).
    fn ref_strong_p0(p2: i32, p1: i32, p0: i32, q0: i32, q1: i32, tc: i32) -> i32 {
        ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3).clamp(p0 - 2 * tc, p0 + 2 * tc) // (8-389)
    }

    fn ref_strong_q0(p1: i32, p0: i32, q0: i32, q1: i32, q2: i32, tc: i32) -> i32 {
        ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3).clamp(q0 - 2 * tc, q0 + 2 * tc) // (8-392)
    }

    fn ref_weak_delta(q0: i32, p0: i32, q1: i32, p1: i32, tc: i32) -> Option<i32> {
        let d = (9 * (q0 - p0) - 3 * (q1 - p1) + 8) >> 4; // (8-395)
        if d.abs() >= tc * 10 {
            return None; // the line is real content: left alone
        }
        Some(d.clamp(-tc, tc)) // (8-396)
    }

    fn ref_chroma_delta(q0: i32, p0: i32, q1: i32, p1: i32, tc: i32) -> i32 {
        ((((q0 - p0) << 2) + p1 - q1 + 4) >> 3).clamp(-tc, tc) // (8-403)
    }

    #[test]
    fn intra_step_edge_filters_strong_exact() {
        // 2x1 blocks (16x8 luma): 100 left, 106 right, both intra, TU edge at x = 8.
        let (mut y, mut u, mut v) = (plane(16, 8), plane(8, 4), plane(8, 4));
        fill(&mut y, |x, _| if x < 8 { 100 } else { 106 });
        fill(&mut u, |_, _| 128);
        fill(&mut v, |_, _| 128);
        let mut maps = blank_maps(2, 1);
        maps.intra = vec![true, true];
        maps.tu_edge_v[1] = true;
        deblock_picture(&mut y, &mut u, &mut v, &[32, 32], &maps).unwrap();
        // qPL = (32+32+1)>>1 = 32; beta = BETA_TABLE[32] = 26; tC: Q = 32+2*(2-1) = 34 -> TC_TABLE[34] = 3.
        // d = 0 < 26; strong: 0 < 26>>2, 0 < 26>>3, |100-106| = 6 < (5*3+1)>>1 = 8, on both outer lines.
        // t2 = 6: p0' = 822>>3 = 102, p1' = 408>>2 = 102, p2' = 810>>3 = 101,
        //         q0' = 834>>3 = 104, q1' = 420>>2 = 105, q2' = 846>>3 = 105.
        assert_eq!(&row(&y, 0)[4..12], &[100, 101, 102, 102, 104, 105, 105, 106]);
        assert_eq!(&row(&y, 7)[4..12], &[100, 101, 102, 102, 104, 105, 105, 106]);
        assert_eq!((at(&y, 0, 0), at(&y, 15, 0)), (100, 106)); // samples past p3/q3 are untouched
        assert!(u.data.iter().all(|&s| s == 0 || s == 128)); // x = 8 misses the 16-grid: chroma idle
        assert!(v.data.iter().all(|&s| s == 0 || s == 128));
    }

    #[test]
    fn silent_edge_leaves_step_alone() {
        // Same step, but inter blocks with no TU/PU edge and identical motion: bS = 0 everywhere.
        let (mut y, mut u, mut v) = (plane(16, 8), plane(8, 4), plane(8, 4));
        fill(&mut y, |x, _| if x < 8 { 100 } else { 106 });
        let before = y.data.clone();
        let mut maps = blank_maps(2, 1);
        maps.pu_edge_v[1] = true; // flagged PU edge, yet same ref and MV: still bS = 0
        deblock_picture(&mut y, &mut u, &mut v, &[32, 32], &maps).unwrap();
        assert_eq!(y.data, before);
    }

    #[test]
    fn beta_gate_keeps_high_contrast_edge() {
        // qPL = 32 -> beta = 26. p-side texture p3..p0 = 10/60/10/50, q-side flat 200, bS = 2.
        // dp0 = |60-2*10+50| = 90 per outer line, so d = 180 >= 26 -> dE = 0, untouched.
        let (mut y, mut u, mut v) = (plane(16, 8), plane(8, 4), plane(8, 4));
        fill(&mut y, |x, _| match x {
            0..=3 => 10,
            4 => 10,
            5 => 60,
            6 => 10,
            7 => 50,
            _ => 200,
        });
        fill(&mut u, |_, _| 128);
        let (before_y, before_u) = (y.data.clone(), u.data.clone());
        let mut maps = blank_maps(2, 1);
        maps.intra = vec![true, true];
        maps.tu_edge_v[1] = true;
        deblock_picture(&mut y, &mut u, &mut v, &[32, 32], &maps).unwrap();
        assert_eq!(y.data, before_y);
        assert_eq!(u.data, before_u);
    }

    #[test]
    fn chroma_filters_only_intra_edges() {
        // 3x1 blocks (24x8 luma, 12x4 chroma): luma edge x = 16 sits on the chroma 16-grid.
        let build = || {
            let (mut y, mut u, mut v) = (plane(24, 8), plane(12, 4), plane(12, 4));
            fill(&mut y, |x, _| if x < 16 { 100 } else { 106 });
            fill(&mut u, |x, _| if x < 8 { 100 } else { 150 });
            fill(&mut v, |x, _| if x < 8 { 100 } else { 150 });
            (y, u, v)
        };
        // Phase 1: inter + TU edge + cbf -> bS = 1: luma strong-filters, chroma stays put.
        let (mut y, mut u, mut v) = build();
        let before_u = u.data.clone();
        let before_v = v.data.clone();
        let mut maps = blank_maps(3, 1);
        maps.tu_edge_v[2] = true;
        maps.cbf[1] = true;
        deblock_picture(&mut y, &mut u, &mut v, &[32, 32, 32], &maps).unwrap();
        assert_eq!(u.data, before_u);
        assert_eq!(v.data, before_v);
        assert_eq!((at(&y, 15, 0), at(&y, 16, 0)), (102, 104)); // smooth step still strong-filters luma
        // Phase 2: intra -> bS = 2: the same chroma edge now filters.
        // qPi = 32 -> QpC = qpc(32) = 31 -> Q = 33 -> tC = 3; delta = 154>>3 = 19 -> clips to 3.
        let (mut y2, mut u2, mut v2) = build();
        let mut maps2 = blank_maps(3, 1);
        maps2.intra = vec![true, true, true];
        maps2.tu_edge_v[2] = true;
        deblock_picture(&mut y2, &mut u2, &mut v2, &[32, 32, 32], &maps2).unwrap();
        assert_eq!((u2.data[u2.idx(7, 0)], u2.data[u2.idx(8, 0)]), (103, 147));
        assert_eq!((v2.data[v2.idx(7, 0)], v2.data[v2.idx(8, 0)]), (103, 147));
    }

    #[test]
    fn thresholds_use_mean_qp_of_both_sides() {
        // Step 100|106, intra + TU edge (bS = 2, strong). A [20,44] averages to qPL = 32 like B [32,32].
        let build = || {
            let (mut y, u, v) = (plane(16, 8), plane(8, 4), plane(8, 4));
            fill(&mut y, |x, _| if x < 8 { 100 } else { 106 });
            (y, u, v)
        };
        let edge = |qp: &[u8]| {
            let (mut y, mut u, mut v) = build();
            let mut maps = blank_maps(2, 1);
            maps.intra = vec![true, true];
            maps.tu_edge_v[1] = true;
            deblock_picture(&mut y, &mut u, &mut v, qp, &maps).unwrap();
            y
        };
        let (ya, yb, yc) = (edge(&[20, 44]), edge(&[32, 32]), edge(&[20, 20]));
        assert_eq!(ya.data, yb.data); // mean QP 32 both: identical pictures
        assert_eq!(at(&ya, 7, 0), 102); // strong p0 with tC = 3
        // C: qPL = 20 -> beta = 10, tC: Q = 22 -> 1. |100-106| = 6 !< (5*1+1)>>1 = 3: weak instead.
        // delta = (9*6-3*6+8)>>4 = 44>>4 = 2 -> clips to [-1,1]: p0 100 -> 101, proving the mean mattered.
        assert_eq!(at(&yc, 7, 0), 101);
    }

    #[test]
    fn bad_inputs_error_without_panic() {
        let (mut y, mut u, mut v) = (plane(16, 8), plane(8, 4), plane(8, 4));
        let mut short = blank_maps(2, 1);
        short.intra.pop();
        assert!(matches!(deblock_picture(&mut y, &mut u, &mut v, &[32, 32], &short), Err(Error::InvalidConfig(_)))); // short maps
        let maps = blank_maps(2, 1);
        assert!(matches!(deblock_picture(&mut y, &mut u, &mut v, &[32], &maps), Err(Error::InvalidConfig(_)))); // short qp_y
        assert!(matches!(deblock_picture(&mut y, &mut u, &mut v, &[32, 32, 32], &maps), Err(Error::InvalidConfig(_)))); // long qp_y
        let wide = blank_maps(3, 1);
        assert!(matches!(deblock_picture(&mut y, &mut u, &mut v, &[32, 32, 32], &wide), Err(Error::InvalidConfig(_)))); // grid vs planes
        let (mut small, mut scu, mut scv) = (plane(8, 8), plane(4, 4), plane(4, 4));
        assert!(matches!(deblock_picture(&mut small, &mut scu, &mut scv, &[32, 32], &maps), Err(Error::InvalidConfig(_)))); // small luma
        assert!(matches!(deblock_picture(&mut y, &mut scu, &mut scv, &[32, 32], &maps), Err(Error::InvalidConfig(_)))); // small chroma
    }

    #[test]
    fn edge_values_match_independent_spec_derivation() {
        // Strong luma: 100|106 step, intra, QP 32 (tC = 3) — (8-389)/(8-392) recomputed locally.
        let (mut y, mut u, mut v) = (plane(16, 8), plane(8, 4), plane(8, 4));
        fill(&mut y, |x, _| if x < 8 { 100 } else { 106 });
        let mut maps = blank_maps(2, 1);
        maps.intra = vec![true, true];
        maps.tu_edge_v[1] = true;
        deblock_picture(&mut y, &mut u, &mut v, &[32, 32], &maps).unwrap();
        assert_eq!(i32::from(at(&y, 7, 0)), ref_strong_p0(100, 100, 100, 106, 106, 3));
        assert_eq!(i32::from(at(&y, 8, 0)), ref_strong_q0(100, 100, 106, 106, 106, 3));
        // Weak luma: 100|120 step (|p0-q0| = 20 breaks strong), inter + cbf, QP 32 (tC = 3).
        let (mut yw, mut uw, mut vw) = (plane(16, 8), plane(8, 4), plane(8, 4));
        fill(&mut yw, |x, _| if x < 8 { 100 } else { 120 });
        let mut mapsw = blank_maps(2, 1);
        mapsw.tu_edge_v[1] = true;
        mapsw.cbf[0] = true;
        deblock_picture(&mut yw, &mut uw, &mut vw, &[32, 32], &mapsw).unwrap();
        let d = ref_weak_delta(120, 100, 120, 100, 3).unwrap(); // (9*20-3*20+8)>>4 = 8 -> clips to 3
        assert_eq!(d, 3);
        assert_eq!((at(&yw, 7, 0), at(&yw, 8, 0)), ((100 + d) as u8, (120 - d) as u8));
        // Chroma: 100|150 step at a bS = 2 edge on the 16-grid, tC = 3 — (8-403) recomputed locally.
        let (mut yc, mut uc, mut vc) = (plane(24, 8), plane(12, 4), plane(12, 4));
        fill(&mut uc, |x, _| if x < 8 { 100 } else { 150 });
        let mut mapsc = blank_maps(3, 1);
        mapsc.intra = vec![true, true, true];
        mapsc.tu_edge_v[2] = true;
        deblock_picture(&mut yc, &mut uc, &mut vc, &[32, 32, 32], &mapsc).unwrap();
        let dc = ref_chroma_delta(150, 100, 150, 100, 3); // 154>>3 = 19 -> clips to 3
        assert_eq!(dc, 3);
        assert_eq!((uc.data[uc.idx(7, 0)], uc.data[uc.idx(8, 0)]), ((100 + dc) as u8, (150 - dc) as u8));
    }
}
