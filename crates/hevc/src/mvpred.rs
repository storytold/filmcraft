//! Motion vector prediction: merge candidates (8.5.3.2.2 - 8.5.3.2.5), AMVP (8.5.3.2.6, 8.5.3.2.7)
//! and temporal motion vector prediction (8.5.3.2.8, 8.5.3.2.9).

use crate::picture::RefPic;
use crate::slicedec::{F_INTRA, MvField, PartMode, SliceDecoder};

/// l0CandIdx / l1CandIdx of Table 8-7.
const COMB: [(usize, usize); 12] = [(0, 1), (1, 0), (0, 2), (2, 0), (1, 2), (2, 1), (0, 3), (3, 0), (1, 3), (3, 1), (2, 3), (3, 2)];

/// Motion vector scaling (8-179 .. 8-183).
pub fn scale_mv(mv: [i16; 2], td: i32, tb: i32) -> [i16; 2] {
    let td = td.clamp(-128, 127);
    let tb = tb.clamp(-128, 127);
    if td == 0 {
        return mv;
    }
    let tx = (16384 + (td.abs() >> 1)) / td;
    let dsf = ((tb * tx + 32) >> 6).clamp(-4096, 4095);
    let s = |c: i16| -> i16 {
        let p = dsf * c as i32;
        (p.signum() * ((p.abs() + 127) >> 8)).clamp(-32768, 32767) as i16
    };
    [s(mv[0]), s(mv[1])]
}

/// Same motion (used lists only).
fn same_motion(a: &MvField, b: &MvField) -> bool {
    for l in 0..2 {
        if a.pred(l) != b.pred(l) {
            return false;
        }
        if a.pred(l) && (a.ref_idx[l] != b.ref_idx[l] || a.mv[l] != b.mv[l]) {
            return false;
        }
    }
    true
}

/// A prediction block: the coding block (`xc`, `yc`, size `ncb`) it belongs to, its own position
/// (`xp`, `yp`), size (`w` x `h`) and index `part_idx` within the coding block, and the coding
/// block's partitioning mode.
#[derive(Clone, Copy)]
pub struct PredBlock {
    pub xc: i32,
    pub yc: i32,
    pub ncb: i32,
    pub xp: i32,
    pub yp: i32,
    pub w: i32,
    pub h: i32,
    pub part_idx: usize,
    pub part: PartMode,
}

impl<'a, S: crate::picture::Sample> SliceDecoder<'a, S> {
    /// Prediction block availability (6.4.2) of the neighbouring location (`xn`, `yn`).
    fn pb_avail(&self, pb: &PredBlock, xn: i32, yn: i32) -> bool {
        let PredBlock { xc, yc, ncb, xp, yp, w, h, part_idx, .. } = *pb;
        let same_cb = xc <= xn && yc <= yn && xc + ncb > xn && yc + ncb > yn;
        let avail = if !same_cb {
            self.pic.zavail(xp, yp, xn, yn, self.slice_addr)
        } else {
            !((w << 1) == ncb && (h << 1) == ncb && part_idx == 1 && yc + h <= yn && xc + w > xn)
        };
        avail && self.pic.blk_at(xn, yn).flags & F_INTRA == 0
    }

    #[inline]
    fn mv_at(&self, x: i32, y: i32) -> MvField {
        self.pic.mvf[(y as usize >> 2) * self.pic.w4 + (x as usize >> 2)]
    }

    #[inline]
    fn ref_pic(&self, l: usize, idx: i8) -> &RefPic {
        &self.refs[l][idx as usize]
    }

    /// Merge mode motion (8.5.3.2.2).
    pub fn derive_merge(&self, pb: &PredBlock, merge_idx: usize) -> MvField {
        let PredBlock { xc, yc, ncb, w: w0, h: h0, part, .. } = *pb;
        let pml = self.pps.log2_parallel_merge_level;
        // with a parallel merge level above 4x4, the 8x8 coding block's 2Nx2N candidates are shared
        let pb = if pml > 2 && ncb == 8 { PredBlock { xp: xc, yp: yc, w: ncb, h: ncb, part_idx: 0, ..*pb } } else { *pb };
        let PredBlock { xp, yp, w, h, part_idx, .. } = pb;
        let same_mer = |xn: i32, yn: i32| (xp >> pml) == (xn >> pml) && (yp >> pml) == (yn >> pml);
        let avail = |xn: i32, yn: i32| self.pb_avail(&pb, xn, yn) && !same_mer(xn, yn);
        let mut list: [MvField; 5] = [MvField::default(); 5];
        let mut n = 0;
        let max = self.sh.max_num_merge_cand as usize;
        // spatial candidates (8.5.3.2.3)
        let pa1 = (xp - 1, yp + h - 1);
        let av_a1 = avail(pa1.0, pa1.1) && !(matches!(part, PartMode::PNx2N | PartMode::PnLx2N | PartMode::PnRx2N) && part_idx == 1);
        let m_a1 = if av_a1 { self.mv_at(pa1.0, pa1.1) } else { MvField::default() };
        if av_a1 {
            list[n] = m_a1;
            n += 1;
        }
        let pb1 = (xp + w - 1, yp - 1);
        let av_b1 = avail(pb1.0, pb1.1) && !(matches!(part, PartMode::P2NxN | PartMode::P2NxnU | PartMode::P2NxnD) && part_idx == 1);
        let m_b1 = if av_b1 { self.mv_at(pb1.0, pb1.1) } else { MvField::default() };
        let fl_b1 = av_b1 && !(av_a1 && same_motion(&m_a1, &m_b1));
        if fl_b1 {
            list[n] = m_b1;
            n += 1;
        }
        let pb0 = (xp + w, yp - 1);
        let av_b0 = avail(pb0.0, pb0.1);
        let fl_b0 = av_b0 && {
            let m = self.mv_at(pb0.0, pb0.1);
            !(av_b1 && same_motion(&m_b1, &m))
        };
        if fl_b0 {
            list[n] = self.mv_at(pb0.0, pb0.1);
            n += 1;
        }
        let pa0 = (xp - 1, yp + h);
        let av_a0 = avail(pa0.0, pa0.1);
        let fl_a0 = av_a0 && {
            let m = self.mv_at(pa0.0, pa0.1);
            !(av_a1 && same_motion(&m_a1, &m))
        };
        if fl_a0 {
            list[n] = self.mv_at(pa0.0, pa0.1);
            n += 1;
        }
        let pb2 = (xp - 1, yp - 1);
        let av_b2 = avail(pb2.0, pb2.1);
        let fl_b2 = av_b2 && {
            let m = self.mv_at(pb2.0, pb2.1);
            !((av_a1 && same_motion(&m_a1, &m)) || (av_b1 && same_motion(&m_b1, &m))) && (fl_a0 as u32 + av_a1 as u32 + fl_b0 as u32 + fl_b1 as u32) != 4
        };
        if fl_b2 {
            list[n] = self.mv_at(pb2.0, pb2.1);
            n += 1;
        }
        if merge_idx < n {
            return self.finish_merge(list[merge_idx], w0, h0);
        }
        // temporal candidate
        let mut col = MvField::default();
        let mut col_avail = false;
        if let Some(mv) = self.temporal(xp, yp, w, h, 0, 0) {
            col.mv[0] = mv;
            col.ref_idx[0] = 0;
            col_avail = true;
        }
        if self.sh.is_b()
            && let Some(mv) = self.temporal(xp, yp, w, h, 1, 0)
        {
            col.mv[1] = mv;
            col.ref_idx[1] = 0;
            col_avail = true;
        }
        let mut cands: Vec<MvField> = list[..n].to_vec();
        if col_avail {
            cands.push(col);
        }
        if merge_idx < cands.len() {
            return self.finish_merge(cands[merge_idx], w0, h0);
        }
        // combined bi-predictive candidates (8.5.3.2.4)
        let num_orig = cands.len();
        if self.sh.is_b() && num_orig > 1 && num_orig < max {
            let mut comb = 0;
            while comb < num_orig * (num_orig - 1) && cands.len() < max {
                let (i0, i1) = COMB[comb];
                let (c0, c1) = (cands[i0], cands[i1]);
                if c0.pred(0) && c1.pred(1) && (self.ref_pic(0, c0.ref_idx[0]).poc != self.ref_pic(1, c1.ref_idx[1]).poc || c0.mv[0] != c1.mv[1]) {
                    cands.push(MvField { mv: [c0.mv[0], c1.mv[1]], ref_idx: [c0.ref_idx[0], c1.ref_idx[1]] });
                }
                comb += 1;
            }
        }
        if merge_idx < cands.len() {
            return self.finish_merge(cands[merge_idx], w0, h0);
        }
        // zero candidates (8.5.3.2.5)
        let num_ref = if self.sh.is_b() { self.sh.num_ref_idx[0].min(self.sh.num_ref_idx[1]) } else { self.sh.num_ref_idx[0] } as usize;
        let mut zero_idx = 0;
        while cands.len() <= merge_idx {
            let r = if zero_idx < num_ref { zero_idx as i8 } else { 0 };
            cands.push(MvField { mv: [[0; 2]; 2], ref_idx: [r, if self.sh.is_b() { r } else { -1 }] });
            zero_idx += 1;
        }
        self.finish_merge(cands[merge_idx], w0, h0)
    }

    fn finish_merge(&self, mut f: MvField, w: i32, h: i32) -> MvField {
        if f.pred(0) && f.pred(1) && w + h == 12 {
            f.ref_idx[1] = -1;
            f.mv[1] = [0, 0];
        }
        for l in 0..2 {
            if !f.pred(l) {
                f.mv[l] = [0, 0];
            }
        }
        f
    }

    /// Temporal luma motion vector prediction (8.5.3.2.8).
    pub fn temporal(&self, xp: i32, yp: i32, w: i32, h: i32, l: usize, ref_idx: usize) -> Option<[i16; 2]> {
        let col = self.col.as_ref()?;
        let s = self.sps.log2_ctb;
        let (xbr, ybr) = (xp + w, yp + h);
        if (yp >> s) == (ybr >> s) && ybr < self.pic.height as i32 && xbr < self.pic.width as i32 {
            let r = self.col_mv(col, (xbr >> 4) << 4, (ybr >> 4) << 4, l, ref_idx);
            if r.is_some() {
                return r;
            }
        }
        let (xc, yc) = (xp + (w >> 1), yp + (h >> 1));
        self.col_mv(col, (xc >> 4) << 4, (yc >> 4) << 4, l, ref_idx)
    }

    /// Collocated motion vectors (8.5.3.2.9).
    fn col_mv(&self, col: &RefPic, x: i32, y: i32, l: usize, ref_idx: usize) -> Option<[i16; 2]> {
        let f = &col.frame;
        if f.width != self.pic.width || f.height != self.pic.height {
            return None;
        }
        let cm = f.col_mv(x as usize, y as usize);
        if cm.flags & 3 == 0 {
            return None;
        }
        let list_col = if cm.flags & 1 == 0 {
            1
        } else if cm.flags & 2 == 0 {
            0
        } else if self.no_backward_pred {
            l
        } else {
            self.sh.collocated_from_l0 as usize
        };
        let mv = cm.mv[list_col];
        let col_lt = (cm.flags >> (2 + list_col)) & 1 == 1;
        let cur = self.refs[l].get(ref_idx)?;
        if cur.long_term != col_lt {
            return None;
        }
        let col_diff = col.poc - cm.ref_poc[list_col];
        let cur_diff = self.pic.poc - cur.poc;
        if cur.long_term || col_diff == cur_diff { Some(mv) } else { Some(scale_mv(mv, col_diff, cur_diff)) }
    }

    /// AMVP predictor (8.5.3.2.6, 8.5.3.2.7).
    pub fn derive_amvp(&self, pb: &PredBlock, l: usize, ref_idx: usize, mvp_flag: usize) -> [i16; 2] {
        let PredBlock { xp, yp, w, h, .. } = *pb;
        let target = &self.refs[l][ref_idx];
        let (tpoc, tlt) = (target.poc, target.long_term);
        let cur_poc = self.pic.poc;
        let y = 1 - l;
        // exact reference match (no scaling)
        let unscaled = |f: &MvField| -> Option<[i16; 2]> {
            if f.pred(l) && self.ref_pic(l, f.ref_idx[l]).poc == tpoc {
                Some(f.mv[l])
            } else if f.pred(y) && self.ref_pic(y, f.ref_idx[y]).poc == tpoc {
                Some(f.mv[y])
            } else {
                None
            }
        };
        // long-term-compatible match, scaled when both references are short-term
        let scaled = |f: &MvField| -> Option<[i16; 2]> {
            let pick = if f.pred(l) && self.ref_pic(l, f.ref_idx[l]).long_term == tlt {
                Some((f.mv[l], self.ref_pic(l, f.ref_idx[l])))
            } else if f.pred(y) && self.ref_pic(y, f.ref_idx[y]).long_term == tlt {
                Some((f.mv[y], self.ref_pic(y, f.ref_idx[y])))
            } else {
                None
            };
            pick.map(|(mv, r)| if r.poc != tpoc && !r.long_term && !tlt { scale_mv(mv, cur_poc - r.poc, cur_poc - tpoc) } else { mv })
        };
        let pa = [(xp - 1, yp + h), (xp - 1, yp + h - 1)];
        let av_a = [self.pb_avail(pb, pa[0].0, pa[0].1), self.pb_avail(pb, pa[1].0, pa[1].1)];
        let is_scaled = av_a[0] || av_a[1];
        let mut a = None;
        for k in 0..2 {
            if av_a[k] && a.is_none() {
                a = unscaled(&self.mv_at(pa[k].0, pa[k].1));
            }
        }
        if a.is_none() {
            for k in 0..2 {
                if av_a[k] && a.is_none() {
                    a = scaled(&self.mv_at(pa[k].0, pa[k].1));
                }
            }
        }
        let pbk = [(xp + w, yp - 1), (xp + w - 1, yp - 1), (xp - 1, yp - 1)];
        let av_b = [0, 1, 2].map(|k| self.pb_avail(pb, pbk[k].0, pbk[k].1));
        let mut b = None;
        for k in 0..3 {
            if av_b[k] && b.is_none() {
                b = unscaled(&self.mv_at(pbk[k].0, pbk[k].1));
            }
        }
        if !is_scaled && b.is_some() {
            a = b;
        }
        if !is_scaled {
            b = None;
            for k in 0..3 {
                if av_b[k] && b.is_none() {
                    b = scaled(&self.mv_at(pbk[k].0, pbk[k].1));
                }
            }
        }
        let mut list: [[i16; 2]; 3] = [[0; 2]; 3];
        let mut n = 0;
        if let Some(a) = a {
            list[n] = a;
            n += 1;
            if let Some(b) = b
                && b != a
            {
                list[n] = b;
                n += 1;
            }
        } else if let Some(b) = b {
            list[n] = b;
            n += 1;
        }
        if n < 2
            && mvp_flag >= n
            && let Some(c) = self.temporal(xp, yp, w, h, l, ref_idx)
        {
            list[n] = c;
            n += 1;
        }
        let _ = n;
        list[mvp_flag.min(1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mv_scaling() {
        // same distance: unchanged; double distance: doubled
        assert_eq!(scale_mv([8, -4], 2, 2), [8, -4]);
        assert_eq!(scale_mv([8, -4], 1, 2), [16, -8]);
        assert_eq!(scale_mv([8, -4], 2, -2), [-8, 4]);
    }
}
