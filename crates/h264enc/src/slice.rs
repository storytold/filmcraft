//! Slice encoding: macroblock analysis, mode decision, residual coding, reconstruction and entropy coding.

use filmcraft_bitstream::BitWriter;

use crate::cabac::{BinSink, CabacEncoder, CabacEstimator, Contexts, CostTable};
use crate::dsp::{sad, sad16, satd, ssd};
use crate::intra::{Edge4, Edge8, filter8, mode_available, pred_chroma, pred4x4, pred8x8, pred16x16};
use crate::mbinfo::{MbInfo, MbKind, Mv, Part};
use crate::nal::{SliceHeader, SliceType, nal};
use crate::picture::{CHROMA_PAD, Frame, LUMA_PAD, RefPic, avg_into};
use crate::pixel::{Depth, Pixel, QP_PRIME_MAX};
use crate::syntax::{MbCode, NbCtx};
use crate::tables::{BLK4_XY, NUM_CTX, RASTER_TO_BLK4, ZIGZAG4, ZIGZAG8, chroma_qp_prime};
use crate::transform::{
    Deadzone, QuantTables, dequant_chroma_dc, dequant_luma_dc, dequant4, dequant8, fdct4, fdct8, idct4, idct8, quant_chroma_dc, quant_luma_dc, quant4, quant8,
};
use crate::{cabac_mb, cavlc};

/// Encoder tuning derived from the preset.
#[derive(Clone, Debug)]
pub struct EncParams {
    pub mbw: usize,
    pub mbh: usize,
    pub cabac: bool,
    pub t8x8: bool,
    pub i4x4: bool,
    pub i8x8: bool,
    pub partitions: bool,
    pub me_range: i32,
    /// 0 = full-pel, 1 = half-pel, 2 = quarter-pel refinement.
    pub subpel: u8,
    /// Extra quarter-pel refinement rounds.
    pub subpel_rounds: u8,
    pub rd: bool,
    pub chroma_qp_offset: i32,
    pub decimate: bool,
    /// Adaptive 4x4/8x8 transform decision for inter macroblocks.
    pub adaptive_t8: bool,
    /// Always try intra in P/B frames (otherwise only when inter looks poor).
    pub always_intra: bool,
    /// Maximum CAVLC level magnitude (Baseline).
    pub cavlc_clamp: bool,
    pub depth: Depth,
}

/// Per-frame shared encoding state.
pub struct FrameEnc<'a, P> {
    pub p: &'a EncParams,
    pub qt: &'a QuantTables,
    pub cost_tab: &'a CostTable,
    pub st: SliceType,
    pub src: &'a Frame<P>,
    pub l0: Option<&'a RefPic<P>>,
    pub l1: Option<&'a RefPic<P>>,
    /// Slice QP'Y (QPY + QpBdOffsetY); every `qp` in this module is a QP'.
    pub qp: u8,
    pub aq: &'a [i8],
    pub hdr: SliceHeader,
    pub nal_ref_idc: u8,
    pub nal_type: u8,
    pub cabac_table: &'a [(i8, i8); NUM_CTX],
    pub lam: &'a Lambdas,
}

/// Lambda tables indexed by QP'. At higher bit depths QP' = QP + 6 * (BitDepth - 8), so the same QP gets
/// a lambda 4^(BitDepth - 8) times larger, matching the larger SSD (and 2^(BitDepth - 8) for SATD).
pub struct Lambdas {
    /// SATD/SAD-domain lambda, Q8.
    pub sad: [u32; QP_PRIME_MAX + 1],
    /// SSD-domain lambda (for RD), Q8 per 1/256 bit.
    pub ssd: [f64; QP_PRIME_MAX + 1],
}

impl Lambdas {
    pub fn new() -> Self {
        let mut sad = [0u32; QP_PRIME_MAX + 1];
        let mut ssd_ = [0f64; QP_PRIME_MAX + 1];
        for qp in 0..=QP_PRIME_MAX {
            let l = 0.85 * 2f64.powf((qp as f64 - 12.0) / 3.0);
            ssd_[qp] = l;
            sad[qp] = ((l.sqrt()) * 256.0).round().max(16.0) as u32;
        }
        Lambdas { sad, ssd: ssd_ }
    }
}

impl Default for Lambdas {
    fn default() -> Self {
        Self::new()
    }
}

/// Mutable band of reconstructed rows belonging to one slice.
pub struct Band<'b, P> {
    pub y: &'b mut [P],
    pub u: &'b mut [P],
    pub v: &'b mut [P],
    pub ys: usize,
    pub cs: usize,
}

#[derive(Default, Clone, Debug)]
pub struct SliceStats {
    pub bits: u64,
    pub intra: u32,
    pub skip: u32,
    pub inter: u32,
    pub qp_sum: u64,
    pub mbs: u32,
}

pub struct SliceOut {
    pub nal: Vec<u8>,
    #[allow(dead_code)]
    pub stats: SliceStats,
}

#[derive(Clone, Copy)]
struct Avail {
    left: bool,
    top: bool,
    tr: bool,
    tl: bool,
}

struct MbEdges<P> {
    top: [P; 24],
    left: [P; 16],
    tl: P,
    ctop: [[P; 8]; 2],
    cleft: [[P; 8]; 2],
    ctl: [P; 2],
}

#[derive(Clone)]
struct Cand<P> {
    code: MbCode,
    info: MbInfo,
    ry: [P; 256],
    ru: [P; 64],
    rv: [P; 64],
    cost: u64,
}

impl<P: Pixel> Cand<P> {
    fn new() -> Box<Cand<P>> {
        Box::new(Cand {
            code: MbCode::default(),
            info: MbInfo::default(),
            ry: [P::default(); 256],
            ru: [P::default(); 64],
            rv: [P::default(); 64],
            cost: u64::MAX,
        })
    }
}

#[inline]
fn se_bits(v: i32) -> u32 {
    let k = if v > 0 { 2 * v as u32 - 1 } else { (-2 * v) as u32 };
    2 * (31 - (k + 1).leading_zeros()) + 1
}

#[inline]
fn mv_bits(mv: Mv, mvp: Mv) -> u32 {
    se_bits(mv.x as i32 - mvp.x as i32) + se_bits(mv.y as i32 - mvp.y as i32)
}

fn median(a: i16, b: i16, c: i16) -> i16 {
    a.max(b).min(a.min(b).max(c))
}

fn decimate_score(levels: &[i16]) -> u32 {
    const T: [u32; 16] = [3, 2, 2, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut score = 0;
    let mut run = 0usize;
    for &l in levels {
        if l == 0 {
            run += 1;
            continue;
        }
        if l.unsigned_abs() > 1 {
            return 64;
        }
        score += T[run.min(15)];
        run = 0;
    }
    score
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Normal,
    Top16x8,
    Bottom16x8,
    Left8x16,
    Right8x16,
}

pub struct SliceEnc<'a, 'b, P> {
    f: &'a FrameEnc<'a, P>,
    band: Band<'b, P>,
    mbs: &'b mut [MbInfo],
    y0: usize,
    y1: usize,
    slice_no: u16,
    last_qp: u8,
    prev_dqp_nz: bool,
    cabac: Option<CabacEncoder>,
    cavlc: Option<BitWriter>,
    skip_run: u32,
    stats: SliceStats,
}

impl<'a, 'b, P: Pixel> SliceEnc<'a, 'b, P> {
    pub fn new(f: &'a FrameEnc<'a, P>, band: Band<'b, P>, mbs: &'b mut [MbInfo], y0: usize, y1: usize, slice_no: u16) -> Self {
        SliceEnc { f, band, mbs, y0, y1, slice_no, last_qp: f.qp, prev_dqp_nz: false, cabac: None, cavlc: None, skip_run: 0, stats: SliceStats::default() }
    }

    pub fn encode(mut self) -> SliceOut {
        let f = self.f;
        let mbw = f.p.mbw;
        let mut hdr = f.hdr.clone();
        hdr.first_mb = (self.y0 * mbw) as u32;
        let mut bw = BitWriter::new();
        hdr.write(&mut bw);
        if f.p.cabac {
            while !bw.is_byte_aligned() {
                bw.write_bit(true);
            }
            let ctx = Contexts::init(f.cabac_table, f.qp as i32 - f.p.depth.qp_offset);
            self.cabac = Some(CabacEncoder::new(ctx, bw.finish()));
        } else {
            self.cavlc = Some(bw);
        }
        for my in self.y0..self.y1 {
            for mx in 0..mbw {
                self.encode_mb(mx, my);
                if let Some(c) = self.cabac.as_mut() {
                    let last = my + 1 == self.y1 && mx + 1 == mbw;
                    if !last {
                        c.terminate(0);
                    }
                }
            }
        }
        let rbsp = if let Some(c) = self.cabac.take() {
            c.finish()
        } else {
            let mut w = self.cavlc.take().unwrap_or_default();
            if self.skip_run > 0 {
                w.write_ue(self.skip_run);
            }
            w.rbsp_trailing();
            w.finish()
        };
        self.stats.bits = rbsp.len() as u64 * 8;
        SliceOut { nal: nal(f.nal_ref_idc, f.nal_type, &rbsp), stats: self.stats }
    }

    // ------------------------------------------------------------------ neighbour access

    #[inline]
    fn mb(&self, mx: usize, my: usize) -> &MbInfo {
        &self.mbs[(my - self.y0) * self.f.p.mbw + mx]
    }

    fn avail(&self, mx: usize, my: usize) -> Avail {
        let top = my > self.y0;
        Avail { left: mx > 0, top, tr: top && mx + 1 < self.f.p.mbw, tl: top && mx > 0 }
    }

    #[inline]
    fn ry(&self, x: usize, y: usize) -> usize {
        (y - self.y0 * 16) * self.band.ys + LUMA_PAD + x
    }
    #[inline]
    fn rc(&self, x: usize, y: usize) -> usize {
        (y - self.y0 * 8) * self.band.cs + CHROMA_PAD + x
    }

    fn edges(&self, mx: usize, my: usize, av: Avail) -> MbEdges<P> {
        let (px, py) = (mx * 16, my * 16);
        let m = P::of(self.f.p.depth.mid);
        let mut e = MbEdges { top: [m; 24], left: [m; 16], tl: m, ctop: [[m; 8]; 2], cleft: [[m; 8]; 2], ctl: [m; 2] };
        if av.top {
            let i = self.ry(px, py - 1);
            e.top[..16].copy_from_slice(&self.band.y[i..i + 16]);
            if av.tr {
                e.top[16..24].copy_from_slice(&self.band.y[i + 16..i + 24]);
            } else {
                let v = e.top[15];
                e.top[16..24].fill(v);
            }
            for (c, pl) in [&*self.band.u, &*self.band.v].into_iter().enumerate() {
                let i = self.rc(px / 2, py / 2 - 1);
                e.ctop[c].copy_from_slice(&pl[i..i + 8]);
            }
        }
        if av.left {
            for y in 0..16 {
                e.left[y] = self.band.y[self.ry(px - 1, py + y)];
            }
            for (c, pl) in [&*self.band.u, &*self.band.v].into_iter().enumerate() {
                for y in 0..8 {
                    e.cleft[c][y] = pl[self.rc(px / 2 - 1, py / 2 + y)];
                }
            }
        }
        if av.tl {
            e.tl = self.band.y[self.ry(px - 1, py - 1)];
            e.ctl[0] = self.band.u[self.rc(px / 2 - 1, py / 2 - 1)];
            e.ctl[1] = self.band.v[self.rc(px / 2 - 1, py / 2 - 1)];
        }
        e
    }

    /// Neighbouring partition data at 4x4 offset (bx, by) relative to the MB (range -1..=4).
    /// None = not available; Some((ref, mv)) with ref = -1 for intra / list unused.
    fn nb_part(&self, cur: &MbInfo, mask: u16, mx: usize, my: usize, bx: i32, by: i32, list: usize) -> Option<(i8, Mv)> {
        if (0..4).contains(&bx) && (0..4).contains(&by) {
            let r = (by * 4 + bx) as usize;
            if mask & (1 << r) != 0 {
                return Some((cur.ref_at(list, r), cur.mv[list][r]));
            }
            return None;
        }
        let dx = if bx < 0 {
            -1
        } else if bx >= 4 {
            1
        } else {
            0
        };
        let dy = if by < 0 { -1 } else { 0 };
        if by >= 4 || (dy == 0 && dx == 1) {
            return None;
        }
        let nx = mx as i32 + dx;
        let ny = my as i32 + dy;
        if nx < 0 || nx >= self.f.p.mbw as i32 || ny < self.y0 as i32 {
            return None;
        }
        let m = self.mb(nx as usize, ny as usize);
        let r = (((by + 4) % 4) * 4 + (bx + 4) % 4) as usize;
        if m.kind.is_intra() {
            return Some((-1, Mv::ZERO));
        }
        let rf = m.ref_at(list, r);
        Some((rf, if rf >= 0 { m.mv[list][r] } else { Mv::ZERO }))
    }

    /// Motion vector predictor (§8.4.1.3) for a partition at 4x4 coords (x, y) of size (w, h) (in 4x4 units).
    fn mvp(&self, cur: &MbInfo, mask: u16, mx: usize, my: usize, x: i32, y: i32, w: i32, list: usize, rf: i8, shape: Shape) -> Mv {
        let a = self.nb_part(cur, mask, mx, my, x - 1, y, list);
        let b = self.nb_part(cur, mask, mx, my, x, y - 1, list);
        let mut c = self.nb_part(cur, mask, mx, my, x + w, y - 1, list);
        if c.is_none() {
            c = self.nb_part(cur, mask, mx, my, x - 1, y - 1, list);
        }
        let (mut b, mut c) = (b, c);
        if b.is_none() && c.is_none() && a.is_some() {
            b = a;
            c = a;
        }
        let a = a.unwrap_or((-1, Mv::ZERO));
        let b = b.unwrap_or((-1, Mv::ZERO));
        let c = c.unwrap_or((-1, Mv::ZERO));
        match shape {
            Shape::Top16x8 if b.0 == rf => return b.1,
            Shape::Bottom16x8 if a.0 == rf => return a.1,
            Shape::Left8x16 if a.0 == rf => return a.1,
            Shape::Right8x16 if c.0 == rf => return c.1,
            _ => {}
        }
        let n = (a.0 == rf) as u32 + (b.0 == rf) as u32 + (c.0 == rf) as u32;
        if n == 1 {
            if a.0 == rf {
                return a.1;
            }
            if b.0 == rf {
                return b.1;
            }
            return c.1;
        }
        Mv { x: median(a.1.x, b.1.x, c.1.x), y: median(a.1.y, b.1.y, c.1.y) }
    }

    fn pskip_mv(&self, mx: usize, my: usize) -> Mv {
        let cur = MbInfo::default();
        let a = self.nb_part(&cur, 0, mx, my, -1, 0, 0);
        let b = self.nb_part(&cur, 0, mx, my, 0, -1, 0);
        match (a, b) {
            (None, _) | (_, None) => Mv::ZERO,
            (Some((0, m)), _) if m == Mv::ZERO => Mv::ZERO,
            (_, Some((0, m))) if m == Mv::ZERO => Mv::ZERO,
            _ => self.mvp(&cur, 0, mx, my, 0, 0, 4, 0, 0, Shape::Normal),
        }
    }

    /// Whether a luma block at picture (x, y) of size w x h with `mv` stays within the padded reference.
    fn mv_ok(&self, x: usize, y: usize, w: usize, h: usize, mv: Mv) -> bool {
        let pw = (self.f.p.mbw * 16) as i32;
        let ph = (self.f.p.mbh * 16) as i32;
        let m = LUMA_PAD as i32 - 20;
        let x0 = x as i32 + (mv.x as i32 >> 2);
        let y0 = y as i32 + (mv.y as i32 >> 2);
        mv.x.unsigned_abs() < 2000 && mv.y.unsigned_abs() < 2000 && x0 >= -m && y0 >= -m && x0 + w as i32 <= pw + m && y0 + h as i32 <= ph + m
    }

    // ------------------------------------------------------------------ macroblock

    fn encode_mb(&mut self, mx: usize, my: usize) {
        let f = self.f;
        let mbw = f.p.mbw;
        let aq = if f.aq.is_empty() { 0 } else { f.aq[my * mbw + mx] as i32 };
        let qp = (f.qp as i32 + aq).clamp(0, 51 + f.p.depth.qp_offset) as u8;
        let av = self.avail(mx, my);
        let (px, py) = (mx * 16, my * 16);
        let mut src_y = [P::default(); 256];
        let mut src_u = [P::default(); 64];
        let mut src_v = [P::default(); 64];
        {
            let s = &f.src.y;
            for r in 0..16 {
                let i = s.idx(px as isize, (py + r) as isize);
                src_y[r * 16..r * 16 + 16].copy_from_slice(&s.data[i..i + 16]);
            }
            for r in 0..8 {
                let i = f.src.u.idx((px / 2) as isize, (py / 2 + r) as isize);
                src_u[r * 8..r * 8 + 8].copy_from_slice(&f.src.u.data[i..i + 8]);
                let i = f.src.v.idx((px / 2) as isize, (py / 2 + r) as isize);
                src_v[r * 8..r * 8 + 8].copy_from_slice(&f.src.v.data[i..i + 8]);
            }
        }
        let src = Src { y: src_y, u: src_u, v: src_v };
        let edges = self.edges(mx, my, av);
        let best = match f.st {
            SliceType::I => self.analyse_intra(mx, my, qp, av, &edges, &src, u64::MAX),
            SliceType::P => self.analyse_p(mx, my, qp, av, &edges, &src),
            SliceType::B => self.analyse_b(mx, my, qp, av, &edges, &src),
        };
        self.commit(mx, my, qp, *best);
    }

    fn commit(&mut self, mx: usize, my: usize, qp: u8, mut c: Cand<P>) {
        let (px, py) = (mx * 16, my * 16);
        // reconstruction
        for r in 0..16 {
            let i = self.ry(px, py + r);
            self.band.y[i..i + 16].copy_from_slice(&c.ry[r * 16..r * 16 + 16]);
        }
        for r in 0..8 {
            let i = self.rc(px / 2, py / 2 + r);
            self.band.u[i..i + 8].copy_from_slice(&c.ru[r * 8..r * 8 + 8]);
            self.band.v[i..i + 8].copy_from_slice(&c.rv[r * 8..r * 8 + 8]);
        }
        let kind = c.code.kind;
        let has_dqp = !kind.is_skip() && (c.code.cbp != 0 || kind == MbKind::I16x16);
        let prev_dqp_nz = self.prev_dqp_nz;
        if has_dqp {
            // mb_qp_delta lies in -(26 + QpBdOffset/2)..=25 + QpBdOffset/2 and wraps modulo 52 + QpBdOffset (7.4.5).
            let off = self.f.p.depth.qp_offset;
            let mut d = qp as i32 - self.last_qp as i32;
            if d < -(26 + off / 2) {
                d += 52 + off;
            } else if d > 25 + off / 2 {
                d -= 52 + off;
            }
            c.code.qp_delta = d;
            self.prev_dqp_nz = d != 0;
            self.last_qp = qp;
        } else {
            c.code.qp_delta = 0;
            self.prev_dqp_nz = false;
        }
        c.info.qp = self.last_qp;
        c.info.slice = self.slice_no + 1;
        c.info.kind = kind;
        c.info.cbp = if kind.is_skip() { 0 } else { c.code.cbp };
        self.stats.mbs += 1;
        self.stats.qp_sum += c.info.qp as u64;
        if kind.is_intra() {
            self.stats.intra += 1;
        } else if kind.is_skip() {
            self.stats.skip += 1;
        } else {
            self.stats.inter += 1;
        }
        let idx = (my - self.y0) * self.f.p.mbw + mx;
        self.mbs[idx] = c.info;
        // entropy coding
        let av = self.avail(mx, my);
        let left = if av.left { Some(&self.mbs[idx - 1]) } else { None };
        let top = if av.top { Some(&self.mbs[idx - self.f.p.mbw]) } else { None };
        let nb = NbCtx { left, top, cur: &self.mbs[idx], slice_type: self.f.st, prev_dqp_nz, t8x8_mode: self.f.p.t8x8 };
        if let Some(cab) = self.cabac.as_mut() {
            if nb.slice_type != SliceType::I {
                cabac_mb::write_skip_flag(cab, &nb, kind.is_skip());
            }
            if !kind.is_skip() {
                cabac_mb::write_mb(cab, &nb, &c.code);
            }
        } else if let Some(w) = self.cavlc.as_mut() {
            if kind.is_skip() {
                self.skip_run += 1;
            } else {
                if nb.slice_type != SliceType::I {
                    w.write_ue(self.skip_run);
                    self.skip_run = 0;
                }
                cavlc::write_mb(w, &nb, &c.code);
            }
        }
    }

    /// Estimated bits (1/256 units) of coding candidate `c` at the current entropy state.
    fn est_bits(&self, mx: usize, my: usize, c: &Cand<P>, qp: u8) -> u64 {
        let av = self.avail(mx, my);
        let idx = (my - self.y0) * self.f.p.mbw + mx;
        let left = if av.left { Some(&self.mbs[idx - 1]) } else { None };
        let top = if av.top { Some(&self.mbs[idx - self.f.p.mbw]) } else { None };
        let mut info = c.info;
        info.kind = c.code.kind;
        info.cbp = c.code.cbp;
        let mut code = c.code.clone();
        code.qp_delta = qp as i32 - self.last_qp as i32;
        let nb = NbCtx { left, top, cur: &info, slice_type: self.f.st, prev_dqp_nz: self.prev_dqp_nz, t8x8_mode: self.f.p.t8x8 };
        if let Some(cab) = self.cabac.as_ref() {
            let mut est = CabacEstimator::new(cab.ctx.clone(), self.f.cost_tab);
            if nb.slice_type != SliceType::I {
                cabac_mb::write_skip_flag(&mut est, &nb, code.kind.is_skip());
            }
            if !code.kind.is_skip() {
                cabac_mb::write_mb(&mut est, &nb, &code);
            }
            est.cost
        } else {
            if code.kind.is_skip() {
                return 256;
            }
            let mut w = BitWriter::new();
            if nb.slice_type != SliceType::I {
                w.write_ue(self.skip_run);
            }
            cavlc::write_mb(&mut w, &nb, &code);
            w.bit_len() as u64 * 256
        }
    }

    fn rd_cost(&self, mx: usize, my: usize, c: &Cand<P>, src: &Src<P>, qp: u8) -> u64 {
        let d = ssd(&src.y, 16, &c.ry, 16, 16, 16) + ssd(&src.u, 8, &c.ru, 8, 8, 8) + ssd(&src.v, 8, &c.rv, 8, 8, 8);
        let bits = self.est_bits(mx, my, c, qp);
        let lam = self.f.lam.ssd[qp as usize];
        (d as f64 * 256.0 + lam * bits as f64) as u64
    }

    // ------------------------------------------------------------------ residual coding helpers

    /// Code chroma residual for prediction `pu`, `pv`; fills code/info and reconstruction. Returns chroma cbp.
    fn code_chroma(&self, src: &Src<P>, pu: &[P; 64], pv: &[P; 64], qp: u8, intra: bool, c: &mut Cand<P>) -> u8 {
        let qt = self.f.qt;
        let dp = self.f.p.depth;
        let qpc = chroma_qp_prime(qp as i32, self.f.p.chroma_qp_offset, dp.qp_offset);
        let dz = if intra { Deadzone::INTRA } else { Deadzone::INTER };
        let mut dc_any = false;
        let mut ac_any = false;
        let mut coefs = [[[0i32; 16]; 4]; 2];
        let mut dcl = [[0i32; 4]; 2];
        for comp in 0..2 {
            let (s, p) = if comp == 0 { (&src.u, pu) } else { (&src.v, pv) };
            let mut dc = [0i32; 4];
            let mut ac_score = 0;
            for b in 0..4 {
                let (bx, by) = ((b & 1) * 4, (b >> 1) * 4);
                let mut d = [0i32; 16];
                for y in 0..4 {
                    for x in 0..4 {
                        d[y * 4 + x] = s[(by + y) * 8 + bx + x].get() - p[(by + y) * 8 + bx + x].get();
                    }
                }
                let mut t = fdct4(&d);
                dc[b] = t[0];
                quant4(&mut t, qt, qpc, dz, true);
                if self.f.p.cavlc_clamp {
                    for v in t.iter_mut().skip(1) {
                        *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
                    }
                }
                let mut scan = [0i16; 16];
                for k in 1..16 {
                    scan[k] = t[ZIGZAG4[k]] as i16;
                }
                ac_score += decimate_score(&scan[1..]);
                c.code.cac[comp * 4 + b] = scan;
                coefs[comp][b] = t;
            }
            if !intra && self.f.p.decimate && ac_score < 7 {
                for b in 0..4 {
                    c.code.cac[comp * 4 + b] = [0; 16];
                    for k in 1..16 {
                        coefs[comp][b][k] = 0;
                    }
                }
            }
            let (mut lv, _) = quant_chroma_dc(&dc, qt, qpc, dz);
            if self.f.p.cavlc_clamp {
                for v in lv.iter_mut() {
                    *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
                }
            }
            dcl[comp] = lv;
            for b in 0..4 {
                c.code.cdc[comp][b] = lv[b] as i16;
            }
            dc_any |= lv.iter().any(|&v| v != 0);
            ac_any |= c.code.cac[comp * 4..comp * 4 + 4].iter().any(|b| b.iter().any(|&v| v != 0));
        }
        let cbp = if ac_any {
            2
        } else if dc_any {
            1
        } else {
            0
        };
        c.info.dc_cbf &= 1;
        for comp in 0..2 {
            let p = if comp == 0 { pu } else { pv };
            let dcq = dequant_chroma_dc(&dcl[comp], qt, qpc);
            if dcl[comp].iter().any(|&v| v != 0) {
                c.info.dc_cbf |= 2 << comp;
            }
            for b in 0..4 {
                let (bx, by) = ((b & 1) * 4, (b >> 1) * 4);
                let mut t = coefs[comp][b];
                let nz = if cbp == 2 { t.iter().skip(1).filter(|&&v| v != 0).count() as u8 } else { 0 };
                c.info.nnz_c[comp * 4 + b] = nz;
                if cbp != 2 {
                    t = [0; 16];
                } else {
                    dequant4(&mut t, qt, qpc, true);
                }
                t[0] = dcq[b];
                let rec = if comp == 0 { &mut c.ru } else { &mut c.rv };
                if nz == 0 {
                    let dcv = (t[0] + 32) >> 6;
                    for y in 0..4 {
                        for x in 0..4 {
                            let i = (by + y) * 8 + bx + x;
                            rec[i] = P::clip(p[i].get() + dcv, dp);
                        }
                    }
                } else {
                    let r = idct4(&t);
                    for y in 0..4 {
                        for x in 0..4 {
                            let i = (by + y) * 8 + bx + x;
                            rec[i] = P::clip(p[i].get() + r[y * 4 + x], dp);
                        }
                    }
                }
            }
        }
        if cbp == 0 {
            c.code.cdc = [[0; 4]; 2];
        }
        if cbp != 2 {
            c.code.cac = [[0; 16]; 8];
        }
        cbp
    }

    /// Inter luma residual with the 4x4 transform. Returns luma cbp.
    fn code_luma4(&self, src: &[P; 256], pred: &[P; 256], qp: u8, dz: Deadzone, decimate: bool, c: &mut Cand<P>) -> u8 {
        let qt = self.f.qt;
        let dp = self.f.p.depth;
        let mut coefs = [[0i32; 16]; 16];
        let mut score8 = [0u32; 4];
        for blk in 0..16 {
            let (bx, by) = BLK4_XY[blk];
            let mut d = [0i32; 16];
            for y in 0..4 {
                for x in 0..4 {
                    let i = (by * 4 + y) * 16 + bx * 4 + x;
                    d[y * 4 + x] = src[i].get() - pred[i].get();
                }
            }
            let mut t = fdct4(&d);
            quant4(&mut t, qt, qp, dz, false);
            if self.f.p.cavlc_clamp {
                for v in t.iter_mut() {
                    *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
                }
            }
            let mut scan = [0i16; 16];
            for k in 0..16 {
                scan[k] = t[ZIGZAG4[k]] as i16;
            }
            score8[blk / 4] += decimate_score(&scan);
            c.code.luma4[blk] = scan;
            coefs[blk] = t;
        }
        let mut cbp = 0u8;
        let total: u32 = score8.iter().sum();
        for b8 in 0..4 {
            let kill = decimate && (score8[b8] < 4 || total < 6);
            for i in 0..4 {
                let blk = b8 * 4 + i;
                if kill {
                    c.code.luma4[blk] = [0; 16];
                    coefs[blk] = [0; 16];
                }
                if c.code.luma4[blk].iter().any(|&v| v != 0) {
                    cbp |= 1 << b8;
                }
            }
        }
        for blk in 0..16 {
            let (bx, by) = BLK4_XY[blk];
            let nz = c.code.luma4[blk].iter().filter(|&&v| v != 0).count() as u8;
            c.info.nnz[by * 4 + bx] = nz;
            let off = by * 4 * 16 + bx * 4;
            if nz == 0 {
                for y in 0..4 {
                    c.ry[off + y * 16..off + y * 16 + 4].copy_from_slice(&pred[off + y * 16..off + y * 16 + 4]);
                }
            } else {
                let mut t = coefs[blk];
                dequant4(&mut t, qt, qp, false);
                let r = idct4(&t);
                for y in 0..4 {
                    for x in 0..4 {
                        let i = off + y * 16 + x;
                        c.ry[i] = P::clip(pred[i].get() + r[y * 4 + x], dp);
                    }
                }
            }
        }
        cbp
    }

    /// Inter luma residual with the 8x8 transform. Returns luma cbp.
    fn code_luma8(&self, src: &[P; 256], pred: &[P; 256], qp: u8, dz: Deadzone, decimate: bool, c: &mut Cand<P>) -> u8 {
        let qt = self.f.qt;
        let dp = self.f.p.depth;
        let mut coefs = [[0i32; 64]; 4];
        let mut score = [0u32; 4];
        for b8 in 0..4 {
            let (ox, oy) = ((b8 & 1) * 8, (b8 >> 1) * 8);
            let mut d = [0i32; 64];
            for y in 0..8 {
                for x in 0..8 {
                    let i = (oy + y) * 16 + ox + x;
                    d[y * 8 + x] = src[i].get() - pred[i].get();
                }
            }
            let mut t = fdct8(&d);
            quant8(&mut t, qt, qp, dz);
            let mut scan = [0i16; 64];
            for k in 0..64 {
                scan[k] = t[ZIGZAG8[k]] as i16;
            }
            score[b8] = decimate_score(&scan);
            c.code.luma8[b8] = scan;
            coefs[b8] = t;
        }
        let total: u32 = score.iter().sum();
        let mut cbp = 0;
        for b8 in 0..4 {
            let (ox, oy) = ((b8 & 1) * 8, (b8 >> 1) * 8);
            if decimate && (score[b8] < 4 || total < 6) {
                c.code.luma8[b8] = [0; 64];
                coefs[b8] = [0; 64];
            }
            let nz = c.code.luma8[b8].iter().filter(|&&v| v != 0).count() as u8;
            let (bx, by) = (ox / 4, oy / 4);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                c.info.nnz[(by + dy) * 4 + bx + dx] = nz;
            }
            if nz == 0 {
                for y in 0..8 {
                    let o = (oy + y) * 16 + ox;
                    c.ry[o..o + 8].copy_from_slice(&pred[o..o + 8]);
                }
                continue;
            }
            cbp |= 1 << b8;
            let mut t = coefs[b8];
            dequant8(&mut t, qt, qp);
            let r = idct8(&t);
            for y in 0..8 {
                for x in 0..8 {
                    let i = (oy + y) * 16 + ox + x;
                    c.ry[i] = P::clip(pred[i].get() + r[y * 8 + x], dp);
                }
            }
        }
        cbp
    }

    /// Code an inter candidate's residual given its luma/chroma prediction.
    fn code_inter(&self, src: &Src<P>, py: &[P; 256], pu: &[P; 64], pv: &[P; 64], qp: u8, c: &mut Cand<P>) {
        let t8_allowed = self.f.p.t8x8;
        let use8 = if t8_allowed {
            if self.f.p.adaptive_t8 {
                // Compare 4x4 and 8x8 transform by SSD + lambda * (approximate) bits.
                let mut c4 = c.clone();
                let cbp4 = self.code_luma4(&src.y, py, qp, Deadzone::INTER, self.f.p.decimate, &mut c4);
                let mut c8 = c.clone();
                let cbp8 = self.code_luma8(&src.y, py, qp, Deadzone::INTER, self.f.p.decimate, &mut c8);
                let lam = self.f.lam.ssd[qp as usize];
                let cost = |cc: &Cand<P>, cbp: u8, eight: bool| {
                    let d = ssd(&src.y, 16, &cc.ry, 16, 16, 16) as f64;
                    let mut bits = 0f64;
                    if eight {
                        for b in 0..4 {
                            if (cbp >> b) & 1 != 0 {
                                bits += cc.code.luma8[b].iter().map(|&v| if v == 0 { 0.4 } else { 3.0 + 2.0 * (v.unsigned_abs() as f64).log2() }).sum::<f64>();
                            }
                        }
                    } else {
                        for b in 0..16 {
                            if (cbp >> (b / 4)) & 1 != 0 {
                                bits += 1.0
                                    + cc.code.luma4[b].iter().map(|&v| if v == 0 { 0.4 } else { 3.0 + 2.0 * (v.unsigned_abs() as f64).log2() }).sum::<f64>();
                            }
                        }
                    }
                    d + lam * bits
                };
                let _ = (cbp4, cbp8);
                cost(&c8, cbp8, true) <= cost(&c4, cbp4, false)
            } else {
                true
            }
        } else {
            false
        };
        let cbpl = if use8 {
            self.code_luma8(&src.y, py, qp, Deadzone::INTER, self.f.p.decimate, c)
        } else {
            self.code_luma4(&src.y, py, qp, Deadzone::INTER, self.f.p.decimate, c)
        };
        c.code.t8x8 = use8 && cbpl != 0;
        c.info.t8x8 = c.code.t8x8;
        let cbpc = self.code_chroma(src, pu, pv, qp, false, c);
        c.code.cbp = cbpl | (cbpc << 4);
    }

    // ------------------------------------------------------------------ intra

    fn pred_intra_mode(&self, mx: usize, my: usize, av: Avail, local: &[i8; 16], bx: usize, by: usize) -> u8 {
        let a = if bx > 0 {
            Some(local[by * 4 + bx - 1])
        } else if av.left {
            Some(self.mb(mx - 1, my).imodes[by * 4 + 3])
        } else {
            None
        };
        let b = if by > 0 {
            Some(local[(by - 1) * 4 + bx])
        } else if av.top {
            Some(self.mb(mx, my - 1).imodes[12 + bx])
        } else {
            None
        };
        match (a, b) {
            (Some(a), Some(b)) => {
                let a = if a < 0 { 2 } else { a };
                let b = if b < 0 { 2 } else { b };
                a.min(b) as u8
            }
            _ => 2,
        }
    }

    /// Luma I16x16 coding. Returns luma cbp (0 or 15).
    fn code_i16(&self, src: &[P; 256], pred: &[P; 256], qp: u8, c: &mut Cand<P>) -> u8 {
        let qt = self.f.qt;
        let dp = self.f.p.depth;
        let mut coefs = [[0i32; 16]; 16];
        let mut dc = [0i32; 16];
        for blk in 0..16 {
            let (bx, by) = BLK4_XY[blk];
            let mut d = [0i32; 16];
            for y in 0..4 {
                for x in 0..4 {
                    let i = (by * 4 + y) * 16 + bx * 4 + x;
                    d[y * 4 + x] = src[i].get() - pred[i].get();
                }
            }
            let mut t = fdct4(&d);
            dc[by * 4 + bx] = t[0];
            quant4(&mut t, qt, qp, Deadzone::INTRA, true);
            if self.f.p.cavlc_clamp {
                for v in t.iter_mut() {
                    *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
                }
            }
            coefs[blk] = t;
        }
        let (mut dcl, _) = quant_luma_dc(&dc, qt, qp, Deadzone::INTRA);
        if self.f.p.cavlc_clamp {
            for v in dcl.iter_mut() {
                *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
            }
        }
        for k in 0..16 {
            c.code.luma_dc[k] = dcl[ZIGZAG4[k]] as i16;
        }
        let ac_any = coefs.iter().any(|t| t[1..].iter().any(|&v| v != 0));
        let dcq = dequant_luma_dc(&dcl, qt, qp);
        c.info.dc_cbf = (c.info.dc_cbf & !1) | dcl.iter().any(|&v| v != 0) as u8;
        for blk in 0..16 {
            let (bx, by) = BLK4_XY[blk];
            let mut t = coefs[blk];
            let mut scan = [0i16; 16];
            for k in 1..16 {
                scan[k] = t[ZIGZAG4[k]] as i16;
            }
            c.code.luma4[blk] = if ac_any { scan } else { [0; 16] };
            let nz = if ac_any { scan.iter().filter(|&&v| v != 0).count() as u8 } else { 0 };
            c.info.nnz[by * 4 + bx] = nz;
            let off = by * 4 * 16 + bx * 4;
            if nz == 0 {
                let dcv = (dcq[by * 4 + bx] + 32) >> 6;
                for y in 0..4 {
                    for x in 0..4 {
                        let i = off + y * 16 + x;
                        c.ry[i] = P::clip(pred[i].get() + dcv, dp);
                    }
                }
            } else {
                dequant4(&mut t, qt, qp, true);
                t[0] = dcq[by * 4 + bx];
                let r = idct4(&t);
                for y in 0..4 {
                    for x in 0..4 {
                        let i = off + y * 16 + x;
                        c.ry[i] = P::clip(pred[i].get() + r[y * 4 + x], dp);
                    }
                }
            }
        }
        if ac_any { 15 } else { 0 }
    }

    fn best_chroma_mode(&self, e: &MbEdges<P>, av: Avail, src: &Src<P>, lam: u32) -> (u8, [P; 64], [P; 64]) {
        let d = self.f.p.depth;
        let mut best = (u32::MAX, 0u8, [P::default(); 64], [P::default(); 64]);
        for mode in 0..4u8 {
            let ok = match mode {
                0 => true,
                1 => av.left,
                2 => av.top,
                _ => av.left && av.top && av.tl,
            };
            if !ok {
                continue;
            }
            let mut pu = [P::default(); 64];
            let mut pv = [P::default(); 64];
            pred_chroma(mode, &e.ctop[0], &e.cleft[0], e.ctl[0], av.top, av.left, d, &mut pu);
            pred_chroma(mode, &e.ctop[1], &e.cleft[1], e.ctl[1], av.top, av.left, d, &mut pv);
            let cost = satd(&src.u, 8, &pu, 8, 8, 8) + satd(&src.v, 8, &pv, 8, 8, 8) + ((lam * (1 + 2 * mode as u32)) >> 8);
            if cost < best.0 {
                best = (cost, mode, pu, pv);
            }
        }
        (best.1, best.2, best.3)
    }

    fn edge4(&self, e: &MbEdges<P>, av: Avail, rec: &[P; 256], bx: usize, by: usize) -> Edge4<P> {
        let mut top = [P::default(); 8];
        let mut left = [P::default(); 4];
        let has_top = by > 0 || av.top;
        let has_left = bx > 0 || av.left;
        let has_tl = if bx > 0 && by > 0 {
            true
        } else if by == 0 && bx > 0 {
            av.top
        } else if bx == 0 && by > 0 {
            av.left
        } else {
            av.tl
        };
        let tr_avail =
            if by == 0 { if bx < 3 { av.top } else { av.tr } } else { bx < 3 && RASTER_TO_BLK4[(by - 1) * 4 + bx + 1] < RASTER_TO_BLK4[by * 4 + bx] };
        if has_top {
            for i in 0..4 {
                top[i] = if by > 0 { rec[(by * 4 - 1) * 16 + bx * 4 + i] } else { e.top[bx * 4 + i] };
            }
            for i in 4..8 {
                top[i] = if tr_avail { if by > 0 { rec[(by * 4 - 1) * 16 + bx * 4 + i] } else { e.top[bx * 4 + i] } } else { top[3] };
            }
        }
        if has_left {
            for i in 0..4 {
                left[i] = if bx > 0 { rec[(by * 4 + i) * 16 + bx * 4 - 1] } else { e.left[by * 4 + i] };
            }
        }
        let tl = if !has_tl {
            P::default()
        } else if bx > 0 && by > 0 {
            rec[(by * 4 - 1) * 16 + bx * 4 - 1]
        } else if by == 0 && bx > 0 {
            e.top[bx * 4 - 1]
        } else if bx == 0 && by > 0 {
            e.left[by * 4 - 1]
        } else {
            e.tl
        };
        Edge4 { top, left, tl, has_top, has_left, has_tl }
    }

    fn edge8(&self, e: &MbEdges<P>, av: Avail, rec: &[P; 256], b8: usize) -> Edge8<P> {
        let (x8, y8) = (b8 & 1, b8 >> 1);
        let mut top = [P::default(); 16];
        let mut left = [P::default(); 8];
        let has_top = y8 > 0 || av.top;
        let has_left = x8 > 0 || av.left;
        let has_tl = match b8 {
            0 => av.tl,
            1 => av.top,
            2 => av.left,
            _ => true,
        };
        let tr_avail = match b8 {
            0 => av.top,
            1 => av.tr,
            2 => true,
            _ => false,
        };
        if has_top {
            for i in 0..8 {
                top[i] = if y8 > 0 { rec[7 * 16 + x8 * 8 + i] } else { e.top[x8 * 8 + i] };
            }
            for i in 8..16 {
                top[i] = if tr_avail { if y8 > 0 { rec[7 * 16 + x8 * 8 + i] } else { e.top[x8 * 8 + i] } } else { top[7] };
            }
        }
        if has_left {
            for i in 0..8 {
                left[i] = if x8 > 0 { rec[(y8 * 8 + i) * 16 + 7] } else { e.left[y8 * 8 + i] };
            }
        }
        let tl = if !has_tl {
            P::default()
        } else {
            match b8 {
                0 => e.tl,
                1 => e.top[7],
                2 => e.left[7],
                _ => rec[7 * 16 + 7],
            }
        };
        Edge8 { top, left, tl, has_top, has_left, has_tl }
    }

    /// Intra 4x4 analysis + coding. Returns cost (SATD domain) or None if it exceeded `limit`.
    #[allow(clippy::too_many_arguments)]
    fn intra4x4(&self, mx: usize, my: usize, qp: u8, av: Avail, e: &MbEdges<P>, src: &Src<P>, c: &mut Cand<P>, limit: u64) -> Option<u64> {
        let qt = self.f.qt;
        let dp = self.f.p.depth;
        let lam = self.f.lam.sad[qp as usize];
        let mut modes = [-1i8; 16];
        let mut total = ((lam * 24) >> 8) as u64;
        for blk in 0..16 {
            let (bx, by) = BLK4_XY[blk];
            let ed = self.edge4(e, av, &c.ry, bx, by);
            let pm = self.pred_intra_mode(mx, my, av, &modes, bx, by);
            let mut srcb = [P::default(); 16];
            for y in 0..4 {
                srcb[y * 4..y * 4 + 4].copy_from_slice(&src.y[(by * 4 + y) * 16 + bx * 4..(by * 4 + y) * 16 + bx * 4 + 4]);
            }
            let mut best = (u32::MAX, 2u8, [P::default(); 16]);
            for mode in 0..9u8 {
                if !mode_available(mode, ed.has_top, ed.has_left, ed.has_tl) {
                    continue;
                }
                let mut p = [P::default(); 16];
                pred4x4(mode, &ed, dp, &mut p);
                let bits = if mode == pm { 1 } else { 4 };
                let cost = satd(&srcb, 4, &p, 4, 4, 4) + ((lam * bits) >> 8);
                if cost < best.0 {
                    best = (cost, mode, p);
                }
            }
            total += best.0 as u64;
            if total > limit {
                return None;
            }
            let (mode, p) = (best.1, best.2);
            modes[by * 4 + bx] = mode as i8;
            c.code.ipred[blk] = if mode == pm {
                -1
            } else if mode < pm {
                mode as i8
            } else {
                mode as i8 - 1
            };
            // code block
            let mut d = [0i32; 16];
            for i in 0..16 {
                d[i] = srcb[i].get() - p[i].get();
            }
            let mut t = fdct4(&d);
            let nz = quant4(&mut t, qt, qp, Deadzone::INTRA, false);
            if self.f.p.cavlc_clamp {
                for v in t.iter_mut() {
                    *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
                }
            }
            let mut scan = [0i16; 16];
            for k in 0..16 {
                scan[k] = t[ZIGZAG4[k]] as i16;
            }
            c.code.luma4[blk] = scan;
            c.info.nnz[by * 4 + bx] = nz as u8;
            let off = by * 4 * 16 + bx * 4;
            if nz == 0 {
                for y in 0..4 {
                    c.ry[off + y * 16..off + y * 16 + 4].copy_from_slice(&p[y * 4..y * 4 + 4]);
                }
            } else {
                dequant4(&mut t, qt, qp, false);
                let r = idct4(&t);
                for y in 0..4 {
                    for x in 0..4 {
                        c.ry[off + y * 16 + x] = P::clip(p[y * 4 + x].get() + r[y * 4 + x], dp);
                    }
                }
            }
        }
        c.info.imodes = modes;
        let mut cbp = 0;
        for b8 in 0..4 {
            if (0..4).any(|i| c.code.luma4[b8 * 4 + i].iter().any(|&v| v != 0)) {
                cbp |= 1 << b8;
            }
        }
        c.code.cbp = cbp;
        c.code.kind = MbKind::I4x4;
        c.code.t8x8 = false;
        c.info.t8x8 = false;
        Some(total)
    }

    /// Intra 8x8 analysis + coding.
    #[allow(clippy::too_many_arguments)]
    fn intra8x8(&self, mx: usize, my: usize, qp: u8, av: Avail, e: &MbEdges<P>, src: &Src<P>, c: &mut Cand<P>, limit: u64) -> Option<u64> {
        let qt = self.f.qt;
        let dp = self.f.p.depth;
        let lam = self.f.lam.sad[qp as usize];
        let mut modes = [-1i8; 16];
        let mut total = ((lam * 12) >> 8) as u64;
        for b8 in 0..4 {
            let (x8, y8) = (b8 & 1, b8 >> 1);
            let ed = self.edge8(e, av, &c.ry, b8);
            let fe = filter8(&ed);
            let pm = self.pred_intra_mode(mx, my, av, &modes, x8 * 2, y8 * 2);
            let mut srcb = [P::default(); 64];
            for y in 0..8 {
                srcb[y * 8..y * 8 + 8].copy_from_slice(&src.y[(y8 * 8 + y) * 16 + x8 * 8..(y8 * 8 + y) * 16 + x8 * 8 + 8]);
            }
            let mut best = (u32::MAX, 2u8, [P::default(); 64]);
            for mode in 0..9u8 {
                if !mode_available(mode, ed.has_top, ed.has_left, ed.has_tl) {
                    continue;
                }
                let mut p = [P::default(); 64];
                pred8x8(mode, &fe, dp, &mut p);
                let bits = if mode == pm { 1 } else { 4 };
                let cost = satd(&srcb, 8, &p, 8, 8, 8) + ((lam * bits) >> 8);
                if cost < best.0 {
                    best = (cost, mode, p);
                }
            }
            total += best.0 as u64;
            if total > limit {
                return None;
            }
            let (mode, p) = (best.1, best.2);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                modes[(y8 * 2 + dy) * 4 + x8 * 2 + dx] = mode as i8;
            }
            c.code.ipred[b8] = if mode == pm {
                -1
            } else if mode < pm {
                mode as i8
            } else {
                mode as i8 - 1
            };
            let mut d = [0i32; 64];
            for i in 0..64 {
                d[i] = srcb[i].get() - p[i].get();
            }
            let mut t = fdct8(&d);
            let nz = quant8(&mut t, qt, qp, Deadzone::INTRA);
            let mut scan = [0i16; 64];
            for k in 0..64 {
                scan[k] = t[ZIGZAG8[k]] as i16;
            }
            c.code.luma8[b8] = scan;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                c.info.nnz[(y8 * 2 + dy) * 4 + x8 * 2 + dx] = nz as u8;
            }
            let off = y8 * 8 * 16 + x8 * 8;
            if nz == 0 {
                for y in 0..8 {
                    c.ry[off + y * 16..off + y * 16 + 8].copy_from_slice(&p[y * 8..y * 8 + 8]);
                }
            } else {
                dequant8(&mut t, qt, qp);
                let r = idct8(&t);
                for y in 0..8 {
                    for x in 0..8 {
                        c.ry[off + y * 16 + x] = P::clip(p[y * 8 + x].get() + r[y * 8 + x], dp);
                    }
                }
            }
        }
        c.info.imodes = modes;
        let mut cbp = 0;
        for b8 in 0..4 {
            if c.code.luma8[b8].iter().any(|&v| v != 0) {
                cbp |= 1 << b8;
            }
        }
        c.code.cbp = cbp;
        c.code.kind = MbKind::I8x8;
        c.code.t8x8 = true;
        c.info.t8x8 = true;
        Some(total)
    }

    /// Full intra analysis; returns the best coded intra candidate. `inter_cost` lets P/B skip expensive modes.
    #[allow(clippy::too_many_arguments)]
    fn analyse_intra(&self, mx: usize, my: usize, qp: u8, av: Avail, e: &MbEdges<P>, src: &Src<P>, inter_cost: u64) -> Box<Cand<P>> {
        let lam = self.f.lam.sad[qp as usize];
        let p = self.f.p;
        // I16x16 mode
        let mut top16 = [P::default(); 16];
        top16.copy_from_slice(&e.top[..16]);
        let mut b16 = (u32::MAX, 2u8, [P::default(); 256]);
        for mode in 0..4u8 {
            let ok = match mode {
                0 => av.top,
                1 => av.left,
                2 => true,
                _ => av.top && av.left && av.tl,
            };
            if !ok {
                continue;
            }
            let mut pr = [P::default(); 256];
            pred16x16(mode, &top16, &e.left, e.tl, av.top, av.left, p.depth, &mut pr);
            let cost = satd(&src.y, 16, &pr, 16, 16, 16);
            if cost < b16.0 {
                b16 = (cost, mode, pr);
            }
        }
        let cost16 = b16.0 as u64 + ((lam * 6) >> 8) as u64;
        let (cmode, pu, pv) = self.best_chroma_mode(e, av, src, lam);
        let mk16 = || {
            let mut c = Cand::new();
            c.code.kind = MbKind::I16x16;
            c.code.i16_mode = b16.1;
            c.code.chroma_mode = cmode;
            let cbpl = self.code_i16(&src.y, &b16.2, qp, &mut c);
            let cbpc = self.code_chroma(src, &pu, &pv, qp, true, &mut c);
            c.code.cbp = cbpl | (cbpc << 4);
            c.info.chroma_mode = cmode;
            c.info.imodes = [-1; 16];
            c.cost = cost16;
            c
        };
        let mut best: Option<Box<Cand<P>>> = None;
        let try_nxn = p.i4x4 && (inter_cost == u64::MAX || cost16 < inter_cost + inter_cost / 4 || p.always_intra);
        if !try_nxn {
            return mk16();
        }
        let limit = cost16.min(inter_cost.saturating_add(inter_cost / 8));
        let mut limit = if p.rd { cost16 + cost16 / 4 } else { limit };
        if p.i8x8 && p.t8x8 {
            let mut c = Cand::new();
            if let Some(cost) = self.intra8x8(mx, my, qp, av, e, src, &mut c, limit) {
                c.cost = cost;
                limit = limit.min(cost);
                best = Some(c);
            }
        }
        {
            let mut c = Cand::new();
            if let Some(cost) = self.intra4x4(mx, my, qp, av, e, src, &mut c, limit) {
                c.cost = cost;
                if best.as_ref().is_none_or(|b| cost < b.cost) {
                    best = Some(c);
                }
            }
        }
        let finish = |mut c: Box<Cand<P>>| {
            c.code.chroma_mode = cmode;
            c.info.chroma_mode = cmode;
            let cbpc = self.code_chroma(src, &pu, &pv, qp, true, &mut c);
            c.code.cbp = (c.code.cbp & 15) | (cbpc << 4);
            c
        };
        match best {
            None => mk16(),
            Some(c) => {
                let c = finish(c);
                if p.rd {
                    let c16 = mk16();
                    let (j1, j2) = (self.rd_cost(mx, my, &c, src, qp), self.rd_cost(mx, my, &c16, src, qp));
                    let mut w = if j1 <= j2 { c } else { c16 };
                    w.cost = w.cost.min(cost16);
                    w
                } else if c.cost < cost16 {
                    c
                } else {
                    mk16()
                }
            }
        }
    }

    // ------------------------------------------------------------------ motion estimation

    /// Integer + sub-pel search for a w x h block at picture (bx, by). Returns (mv, cost, satd).
    #[allow(clippy::too_many_arguments)]
    fn search(&self, r: &RefPic<P>, bx: usize, by: usize, w: usize, h: usize, src: &[P], sstride: usize, mvp: Mv, cands: &[Mv], lam: u32) -> (Mv, u32) {
        let pl = &r.frame.y;
        let p = self.f.p;
        let fcost = |mv: Mv| -> u32 {
            let i = pl.idx(bx as isize + (mv.x as isize >> 2), by as isize + (mv.y as isize >> 2));
            let d = if w == 16 { sad16(src, sstride, &pl.data[i..], pl.stride, h) } else { sad(src, sstride, &pl.data[i..], pl.stride, w, h) };
            d + ((lam * mv_bits(mv, mvp)) >> 8)
        };
        let mut best = Mv::new((mvp.x as i32 + 2) & !3, (mvp.y as i32 + 2) & !3);
        if !self.mv_ok(bx, by, w, h, best) {
            best = Mv::ZERO;
        }
        let mut bcost = fcost(best);
        for &c in cands {
            let c = Mv::new((c.x as i32 + 2) & !3, (c.y as i32 + 2) & !3);
            if c != best && self.mv_ok(bx, by, w, h, c) {
                let k = fcost(c);
                if k < bcost {
                    bcost = k;
                    best = c;
                }
            }
        }
        // hexagon search (full-pel units = 4 quarter samples)
        let range = p.me_range;
        let centre0 = best;
        const HEX: [(i32, i32); 6] = [(-2, 0), (2, 0), (-1, -2), (1, -2), (-1, 2), (1, 2)];
        for _ in 0..range {
            let mut moved = false;
            let c = best;
            for &(dx, dy) in &HEX {
                let m = Mv::new(c.x as i32 + dx * 4, c.y as i32 + dy * 4);
                if ((m.x as i32 - centre0.x as i32).abs() >> 2) > range || ((m.y as i32 - centre0.y as i32).abs() >> 2) > range || !self.mv_ok(bx, by, w, h, m)
                {
                    continue;
                }
                let k = fcost(m);
                if k < bcost {
                    bcost = k;
                    best = m;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        // small diamond / square refinement
        for _ in 0..2 {
            let c = best;
            for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                let m = Mv::new(c.x as i32 + dx * 4, c.y as i32 + dy * 4);
                if !self.mv_ok(bx, by, w, h, m) {
                    continue;
                }
                let k = fcost(m);
                if k < bcost {
                    bcost = k;
                    best = m;
                }
            }
            if best == c {
                break;
            }
        }
        // sub-pel refinement with SATD
        let mut buf = [P::default(); 256];
        let scost = |mv: Mv, buf: &mut [P; 256]| -> u32 {
            r.mc_luma(bx, by, mv, w, h, buf, 16);
            satd(src, sstride, buf, 16, w, h) + ((lam * mv_bits(mv, mvp)) >> 8)
        };
        let mut bs = scost(best, &mut buf);
        if p.subpel >= 1 {
            for step in [2, 1] {
                if step == 1 && p.subpel < 2 {
                    break;
                }
                let rounds = if step == 1 { 1 + p.subpel_rounds as usize } else { 1 };
                for _ in 0..rounds {
                    let c = best;
                    for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                        let m = Mv::new(c.x as i32 + dx * step, c.y as i32 + dy * step);
                        if !self.mv_ok(bx, by, w, h, m) {
                            continue;
                        }
                        let k = scost(m, &mut buf);
                        if k < bs {
                            bs = k;
                            best = m;
                        }
                    }
                    if best == c {
                        break;
                    }
                }
            }
        }
        (best, bs)
    }

    #[allow(clippy::too_many_arguments)]
    fn pred_block(&self, r: &RefPic<P>, px: usize, py: usize, rect: (usize, usize, usize, usize), mv: Mv, y: &mut [P; 256], u: &mut [P; 64], v: &mut [P; 64]) {
        let (x, yy, w, h) = rect;
        r.mc_luma(px + x, py + yy, mv, w, h, &mut y[yy * 16 + x..], 16);
        let co = (yy / 2) * 8 + x / 2;
        r.mc_chroma((px + x) / 2, (py + yy) / 2, mv, w / 2, h / 2, &mut u[co..], &mut v[co..], 8);
    }

    fn cand_inter_info(c: &mut Cand<P>, part: Part, refs: [[i8; 4]; 2], mvs: [[Mv; 4]; 2], mvds: [[Mv; 4]; 2]) {
        for p in 0..part.count() {
            let (x, y, w, h) = part.rect(p);
            for by in y / 4..(y + h) / 4 {
                for bx in x / 4..(x + w) / 4 {
                    let r = by * 4 + bx;
                    for l in 0..2 {
                        c.info.mv[l][r] = if refs[l][p] >= 0 { mvs[l][p] } else { Mv::ZERO };
                        let d = if refs[l][p] >= 0 { mvds[l][p] } else { Mv::ZERO };
                        c.info.mvd[l][r] = [d.x.unsigned_abs().min(127) as u8, d.y.unsigned_abs().min(127) as u8];
                    }
                }
            }
            for by8 in y / 8..(y + h).div_ceil(8) {
                for bx8 in x / 8..(x + w).div_ceil(8) {
                    for l in 0..2 {
                        c.info.ref_idx[l][by8 * 2 + bx8] = refs[l][p];
                    }
                }
            }
        }
    }

    fn mask_of(part: Part, upto: usize) -> u16 {
        let mut m = 0u16;
        for p in 0..upto {
            let (x, y, w, h) = part.rect(p);
            for by in y / 4..(y + h) / 4 {
                for bx in x / 4..(x + w) / 4 {
                    m |= 1 << (by * 4 + bx);
                }
            }
        }
        m
    }

    fn shape_of(part: Part, p: usize) -> Shape {
        match (part, p) {
            (Part::P16x8, 0) => Shape::Top16x8,
            (Part::P16x8, _) => Shape::Bottom16x8,
            (Part::P8x16, 0) => Shape::Left8x16,
            (Part::P8x16, _) => Shape::Right8x16,
            _ => Shape::Normal,
        }
    }

    /// Search each partition of `part` for list `list` sequentially (mvp depends on earlier partitions).
    #[allow(clippy::too_many_arguments)]
    fn search_part(&self, mx: usize, my: usize, r: &RefPic<P>, part: Part, list: usize, src: &Src<P>, lam: u32, seed: Mv) -> ([Mv; 4], [Mv; 4], u32) {
        let (px, py) = (mx * 16, my * 16);
        let mut info = MbInfo::default();
        let mut mvs = [Mv::ZERO; 4];
        let mut mvps = [Mv::ZERO; 4];
        let mut total = 0;
        for p in 0..part.count() {
            let (x, y, w, h) = part.rect(p);
            let mask = Self::mask_of(part, p);
            let mvp = self.mvp(&info, mask, mx, my, (x / 4) as i32, (y / 4) as i32, (w / 4) as i32, list, 0, Self::shape_of(part, p));
            let (mv, cost) = self.search(r, px + x, py + y, w, h, &src.y[y * 16 + x..], 16, mvp, &[seed, mvs[0]], lam);
            mvs[p] = mv;
            mvps[p] = mvp;
            total += cost;
            // record in scratch info for the next partition's predictor
            for by in y / 4..(y + h) / 4 {
                for bx in x / 4..(x + w) / 4 {
                    info.mv[list][by * 4 + bx] = mv;
                }
            }
            for by8 in y / 8..(y + h).div_ceil(8) {
                for bx8 in x / 8..(x + w).div_ceil(8) {
                    info.ref_idx[list][by8 * 2 + bx8] = 0;
                }
            }
        }
        (mvs, mvps, total)
    }

    fn analyse_p(&self, mx: usize, my: usize, qp: u8, av: Avail, e: &MbEdges<P>, src: &Src<P>) -> Box<Cand<P>> {
        let f = self.f;
        // P slices always have a reference; without one the macroblock is coded intra.
        let Some(r) = f.l0 else { return self.analyse_intra(mx, my, qp, av, e, src, u64::MAX) };
        let lam = f.lam.sad[qp as usize];
        let (px, py) = (mx * 16, my * 16);
        let skip_mv = self.pskip_mv(mx, my);
        let skip_ok = self.mv_ok(px, py, 16, 16, skip_mv);
        // --- try P_Skip early
        let mut skip_pred = None;
        if skip_ok {
            let mut y = [P::default(); 256];
            let mut u = [P::default(); 64];
            let mut v = [P::default(); 64];
            self.pred_block(r, px, py, (0, 0, 16, 16), skip_mv, &mut y, &mut u, &mut v);
            let s = sad16(&src.y, 16, &y, 16, 16);
            let qstep_thresh = (f.qt.v4[(qp % 6) as usize][0] << (qp / 6)) as u32 * 16;
            if s < qstep_thresh * 2 {
                let mut c = Cand::new();
                c.code.kind = MbKind::PInter;
                c.code.part = Part::P16x16;
                self.code_inter(src, &y, &u, &v, qp, &mut c);
                if c.code.cbp == 0 {
                    c.code.kind = MbKind::PSkip;
                    Self::cand_inter_info(&mut c, Part::P16x16, [[0; 4], [-1; 4]], [[skip_mv; 4], [Mv::ZERO; 4]], [[Mv::ZERO; 4]; 2]);
                    c.info.nnz = [0; 16];
                    c.info.nnz_c = [0; 8];
                    c.info.dc_cbf = 0;
                    c.info.t8x8 = false;
                    c.ry = y;
                    c.ru = u;
                    c.rv = v;
                    return c;
                }
            }
            skip_pred = Some((y, u, v));
        }
        // --- 16x16 search
        let mut cands = vec![skip_mv, Mv::ZERO];
        let colo = r.mbs[my * f.p.mbw + mx];
        if colo.ref_idx[0][0] >= 0 {
            cands.push(colo.mv[0][0]);
        }
        let (mv16, mvp16, cost16) = {
            let (m, p, c) = self.search_part(mx, my, r, Part::P16x16, 0, src, lam, skip_mv);
            (m, p, c)
        };
        let _ = cands;
        let mut best_part = Part::P16x16;
        let mut best_mvs = mv16;
        let mut best_mvps = mvp16;
        let mut best_cost = cost16 as u64 + (lam >> 8) as u64;
        if f.p.partitions {
            let big = cost16 as u64 > (256 * 4) << f.p.depth.shift;
            if big {
                for part in [Part::P16x8, Part::P8x16, Part::P8x8] {
                    let (m, p, c) = self.search_part(mx, my, r, part, 0, src, lam, mv16[0]);
                    let extra = match part {
                        Part::P8x8 => 10,
                        _ => 3,
                    };
                    let c = c as u64 + ((lam * extra) >> 8) as u64;
                    if c < best_cost {
                        best_cost = c;
                        best_part = part;
                        best_mvs = m;
                        best_mvps = p;
                    }
                }
            }
        }
        // --- build inter candidate
        let mut ci = Cand::new();
        ci.code.kind = MbKind::PInter;
        ci.code.part = best_part;
        let mut py_ = [P::default(); 256];
        let mut pu = [P::default(); 64];
        let mut pv = [P::default(); 64];
        if let (Part::P16x16, true, Some((a, b, c))) = (best_part, best_mvs[0] == skip_mv, skip_pred) {
            py_ = a;
            pu = b;
            pv = c;
        } else {
            for p in 0..best_part.count() {
                self.pred_block(r, px, py, best_part.rect(p), best_mvs[p], &mut py_, &mut pu, &mut pv);
            }
        }
        let mut mvds = [[Mv::ZERO; 4]; 2];
        for p in 0..best_part.count() {
            mvds[0][p] = Mv::new(best_mvs[p].x as i32 - best_mvps[p].x as i32, best_mvs[p].y as i32 - best_mvps[p].y as i32);
        }
        ci.code.mvd = mvds;
        Self::cand_inter_info(&mut ci, best_part, [[0; 4], [-1; 4]], [best_mvs, [Mv::ZERO; 4]], mvds);
        self.code_inter(src, &py_, &pu, &pv, qp, &mut ci);
        ci.cost = best_cost;
        if ci.code.cbp == 0 && best_part == Part::P16x16 && best_mvs[0] == skip_mv && skip_ok {
            ci.code.kind = MbKind::PSkip;
            ci.info.t8x8 = false;
            ci.info.mvd = [[[0; 2]; 16]; 2];
            return ci;
        }
        // --- intra
        let need_intra = f.p.always_intra || best_cost > (((256 * 3) << f.p.depth.shift) + 16 * lam as u64 / 256) * 2;
        if need_intra {
            let ic = self.analyse_intra(mx, my, qp, av, e, src, best_cost);
            if f.p.rd {
                if self.rd_cost(mx, my, &ic, src, qp) < self.rd_cost(mx, my, &ci, src, qp) {
                    return ic;
                }
            } else if ic.cost < best_cost {
                return ic;
            }
        }
        if f.p.rd && skip_ok && ci.code.cbp != 0 {
            // RD check against P_Skip
            if let Some((y, u, v)) = skip_pred {
                let mut sc = Cand::new();
                sc.code.kind = MbKind::PSkip;
                Self::cand_inter_info(&mut sc, Part::P16x16, [[0; 4], [-1; 4]], [[skip_mv; 4], [Mv::ZERO; 4]], [[Mv::ZERO; 4]; 2]);
                sc.ry = y;
                sc.ru = u;
                sc.rv = v;
                if self.rd_cost(mx, my, &sc, src, qp) < self.rd_cost(mx, my, &ci, src, qp) {
                    return sc;
                }
            }
        }
        ci
    }

    // ------------------------------------------------------------------ B slices

    /// Spatial direct prediction (§8.4.1.2.2) with direct_8x8_inference. Returns per-8x8 refs and mvs.
    fn direct_spatial(&self, mx: usize, my: usize) -> ([[i8; 4]; 2], [[Mv; 4]; 2]) {
        let cur = MbInfo::default();
        let mut refs = [-1i8; 2];
        let mut mvp = [Mv::ZERO; 2];
        for l in 0..2 {
            let a = self.nb_part(&cur, 0, mx, my, -1, 0, l).map_or(-1, |v| v.0);
            let b = self.nb_part(&cur, 0, mx, my, 0, -1, l).map_or(-1, |v| v.0);
            let c = self.nb_part(&cur, 0, mx, my, 4, -1, l).or_else(|| self.nb_part(&cur, 0, mx, my, -1, -1, l)).map_or(-1, |v| v.0);
            let minp = |x: i8, y: i8| if x >= 0 && y >= 0 { x.min(y) } else { x.max(y) };
            refs[l] = minp(a, minp(b, c));
        }
        let zero = refs[0] < 0 && refs[1] < 0;
        if zero {
            refs = [0, 0];
        } else {
            for l in 0..2 {
                if refs[l] >= 0 {
                    mvp[l] = self.mvp(&cur, 0, mx, my, 0, 0, 4, l, refs[l], Shape::Normal);
                }
            }
        }
        let Some(l1) = self.f.l1 else { return ([[0; 4]; 2], [[Mv::ZERO; 4]; 2]) };
        let col = &l1.mbs[my * self.f.p.mbw + mx];
        let mut out_ref = [[-1i8; 4]; 2];
        let mut out_mv = [[Mv::ZERO; 4]; 2];
        for q in 0..4 {
            let corner = [0usize, 3, 12, 15][q];
            let col_zero = if col.kind.is_intra() {
                false
            } else {
                let (rc, mvc) = if col.ref_idx[0][q] >= 0 { (col.ref_idx[0][q], col.mv[0][corner]) } else { (col.ref_idx[1][q], col.mv[1][corner]) };
                rc == 0 && mvc.x.abs() <= 1 && mvc.y.abs() <= 1
            };
            for l in 0..2 {
                out_ref[l][q] = refs[l];
                if refs[l] >= 0 {
                    out_mv[l][q] = if zero || (refs[l] == 0 && col_zero) { Mv::ZERO } else { mvp[l] };
                }
            }
        }
        (out_ref, out_mv)
    }

    /// B 16x8 / 8x16 candidate: search each partition in both lists, choose L0/L1/Bi per partition, then derive
    /// the motion vector predictors in decoding order from the chosen directions.
    #[allow(clippy::too_many_arguments)]
    fn b_partition(&self, mx: usize, my: usize, part: Part, src: &Src<P>, qp: u8, lam: u32, seeds: [Mv; 2]) -> Option<Box<Cand<P>>> {
        let f = self.f;
        let rp = [f.l0?, f.l1?];
        let (px, py) = (mx * 16, my * 16);
        let mut mvs = [[Mv::ZERO; 4]; 2];
        for l in 0..2 {
            mvs[l] = self.search_part(mx, my, rp[l], part, l, src, lam, seeds[l]).0;
        }
        let mut preds = [([P::default(); 256], [P::default(); 64], [P::default(); 64]), ([P::default(); 256], [P::default(); 64], [P::default(); 64])];
        for l in 0..2 {
            let (a, b, c) = &mut preds[l];
            for pi in 0..2 {
                self.pred_block(rp[l], px, py, part.rect(pi), mvs[l][pi], a, b, c);
            }
        }
        let mut bi = preds[0];
        avg_into(&mut bi.0, &preds[1].0);
        avg_into(&mut bi.1, &preds[1].1);
        avg_into(&mut bi.2, &preds[1].2);
        let srcs = [&preds[0], &preds[1], &bi];
        let (mut py_, mut pu, mut pv) = ([P::default(); 256], [P::default(); 64], [P::default(); 64]);
        let mut dirs = [0u8; 4];
        let mut cost = ((lam * 8) >> 8) as u64;
        for pi in 0..2 {
            let (x, y, w, h) = part.rect(pi);
            let mut bestd = (u64::MAX, 0usize);
            for (d, s) in srcs.iter().enumerate() {
                let bits = if d == 2 { 14 } else { 7 };
                let c = satd(&src.y[y * 16 + x..], 16, &s.0[y * 16 + x..], 16, w, h) as u64 + ((lam * bits) >> 8) as u64;
                if c < bestd.0 {
                    bestd = (c, d);
                }
            }
            cost += bestd.0;
            dirs[pi] = bestd.1 as u8;
            let s = srcs[bestd.1];
            for r in y..y + h {
                py_[r * 16 + x..r * 16 + x + w].copy_from_slice(&s.0[r * 16 + x..r * 16 + x + w]);
            }
            for r in y / 2..(y + h) / 2 {
                let o = r * 8 + x / 2;
                pu[o..o + w / 2].copy_from_slice(&s.1[o..o + w / 2]);
                pv[o..o + w / 2].copy_from_slice(&s.2[o..o + w / 2]);
            }
        }
        // predictors in decoding order with the actual per-list references
        let mut info = MbInfo::default();
        let mut refs = [[-1i8; 4]; 2];
        let mut fmvs = [[Mv::ZERO; 4]; 2];
        let mut mvds = [[Mv::ZERO; 4]; 2];
        for pi in 0..2 {
            let (x, y, w, h) = part.rect(pi);
            let mask = Self::mask_of(part, pi);
            for l in 0..2 {
                if dirs[pi] == 2 || dirs[pi] as usize == l {
                    let mvp = self.mvp(&info, mask, mx, my, (x / 4) as i32, (y / 4) as i32, (w / 4) as i32, l, 0, Self::shape_of(part, pi));
                    let mv = mvs[l][pi];
                    mvds[l][pi] = Mv::new(mv.x as i32 - mvp.x as i32, mv.y as i32 - mvp.y as i32);
                    refs[l][pi] = 0;
                    fmvs[l][pi] = mv;
                }
            }
            for l in 0..2 {
                for by in y / 4..(y + h) / 4 {
                    for bx in x / 4..(x + w) / 4 {
                        info.mv[l][by * 4 + bx] = fmvs[l][pi];
                    }
                }
                for by8 in y / 8..(y + h).div_ceil(8) {
                    for bx8 in x / 8..(x + w).div_ceil(8) {
                        info.ref_idx[l][by8 * 2 + bx8] = refs[l][pi];
                    }
                }
            }
        }
        let mut c = Cand::new();
        c.code.kind = MbKind::BInter;
        c.code.part = part;
        c.code.bdir = dirs;
        c.code.mvd = mvds;
        Self::cand_inter_info(&mut c, part, refs, fmvs, mvds);
        self.code_inter(src, &py_, &pu, &pv, qp, &mut c);
        c.cost = cost;
        Some(c)
    }

    fn analyse_b(&self, mx: usize, my: usize, qp: u8, av: Avail, e: &MbEdges<P>, src: &Src<P>) -> Box<Cand<P>> {
        let f = self.f;
        // B slices always have both references; without them the macroblock is coded intra.
        let (Some(r0), Some(r1)) = (f.l0, f.l1) else { return self.analyse_intra(mx, my, qp, av, e, src, u64::MAX) };
        let lam = f.lam.sad[qp as usize];
        let (px, py) = (mx * 16, my * 16);
        // --- direct
        let (dref, dmv) = self.direct_spatial(mx, my);
        let mut direct_ok = true;
        for q in 0..4 {
            let rect = Part::P8x8.rect(q);
            for l in 0..2 {
                if dref[l][q] >= 0 && !self.mv_ok(px + rect.0, py + rect.1, 8, 8, dmv[l][q]) {
                    direct_ok = false;
                }
            }
        }
        let mut best: Option<Box<Cand<P>>> = None;
        if direct_ok {
            let mut y = [P::default(); 256];
            let mut u = [P::default(); 64];
            let mut v = [P::default(); 64];
            let mut ty = [P::default(); 256];
            let mut tu = [P::default(); 64];
            let mut tv = [P::default(); 64];
            for q in 0..4 {
                let rect = Part::P8x8.rect(q);
                let (u0, u1) = (dref[0][q] >= 0, dref[1][q] >= 0);
                if u0 {
                    self.pred_block(r0, px, py, rect, dmv[0][q], &mut y, &mut u, &mut v);
                }
                if u1 {
                    if u0 {
                        self.pred_block(r1, px, py, rect, dmv[1][q], &mut ty, &mut tu, &mut tv);
                        let (x, yy, w, h) = rect;
                        for rr in 0..h {
                            avg_into(&mut y[(yy + rr) * 16 + x..(yy + rr) * 16 + x + w], &ty[(yy + rr) * 16 + x..(yy + rr) * 16 + x + w]);
                        }
                        for rr in 0..h / 2 {
                            let o = (yy / 2 + rr) * 8 + x / 2;
                            avg_into(&mut u[o..o + w / 2], &tu[o..o + w / 2]);
                            avg_into(&mut v[o..o + w / 2], &tv[o..o + w / 2]);
                        }
                    } else {
                        self.pred_block(r1, px, py, rect, dmv[1][q], &mut y, &mut u, &mut v);
                    }
                }
            }
            let mut c = Cand::new();
            c.code.kind = MbKind::BDirect;
            c.code.part = Part::P8x8;
            // store per-8x8 refs/mvs
            let refs = [[dref[0][0], dref[0][1], dref[0][2], dref[0][3]], [dref[1][0], dref[1][1], dref[1][2], dref[1][3]]];
            Self::cand_inter_info(&mut c, Part::P8x8, refs, dmv, [[Mv::ZERO; 4]; 2]);
            c.code.part = Part::P16x16;
            c.cost = satd(&src.y, 16, &y, 16, 16, 16) as u64 + ((lam * 2) >> 8) as u64;
            self.code_inter(src, &y, &u, &v, qp, &mut c);
            if c.code.cbp == 0 {
                c.code.kind = MbKind::BSkip;
                c.info.t8x8 = false;
                // a skip is cheap; accept immediately when the prediction is good
                let thresh = (f.qt.v4[(qp % 6) as usize][0] << (qp / 6)) as u64 * 24;
                if sad16(&src.y, 16, &y, 16, 16) as u64 <= thresh {
                    return c;
                }
            }
            best = Some(c);
        }
        // --- L0 / L1 16x16
        let mut res = [(Mv::ZERO, Mv::ZERO, u32::MAX); 2];
        for (l, rp) in [(0usize, r0), (1usize, r1)] {
            let (m, p, c) = self.search_part(mx, my, rp, Part::P16x16, l, src, lam, if l == 0 { dmv[0][0] } else { dmv[1][0] });
            res[l] = (m[0], p[0], c);
        }
        let mut preds = [([P::default(); 256], [P::default(); 64], [P::default(); 64]), ([P::default(); 256], [P::default(); 64], [P::default(); 64])];
        for (l, rp) in [(0usize, r0), (1usize, r1)] {
            let (a, b, c) = &mut preds[l];
            self.pred_block(rp, px, py, (0, 0, 16, 16), res[l].0, a, b, c);
        }
        let mut bi = preds[0];
        avg_into(&mut bi.0, &preds[1].0);
        avg_into(&mut bi.1, &preds[1].1);
        avg_into(&mut bi.2, &preds[1].2);
        let bits = |l: usize| mv_bits(res[l].0, res[l].1);
        let costs = [
            res[0].2 as u64 + ((lam * 3) >> 8) as u64,
            res[1].2 as u64 + ((lam * 3) >> 8) as u64,
            satd(&src.y, 16, &bi.0, 16, 16, 16) as u64 + ((lam * (bits(0) + bits(1) + 5)) >> 8) as u64,
        ];
        let dir = (0..3).min_by_key(|&d| costs[d]).unwrap_or(0);
        let mut ci = Cand::new();
        ci.code.kind = MbKind::BInter;
        ci.code.part = Part::P16x16;
        ci.code.bdir[0] = dir as u8;
        let mut refs = [[-1i8; 4]; 2];
        let mut mvs = [[Mv::ZERO; 4]; 2];
        let mut mvds = [[Mv::ZERO; 4]; 2];
        for l in 0..2 {
            if dir == 2 || dir == l {
                refs[l] = [0; 4];
                mvs[l] = [res[l].0; 4];
                mvds[l][0] = Mv::new(res[l].0.x as i32 - res[l].1.x as i32, res[l].0.y as i32 - res[l].1.y as i32);
            }
        }
        ci.code.mvd = mvds;
        Self::cand_inter_info(&mut ci, Part::P16x16, refs, mvs, mvds);
        let (py_, pu, pv) = if dir == 2 { bi } else { preds[dir] };
        ci.cost = costs[dir];
        let better_than_direct = best.as_ref().is_none_or(|b| ci.cost < b.cost);
        if better_than_direct || f.p.rd {
            self.code_inter(src, &py_, &pu, &pv, qp, &mut ci);
            best = Some(match best {
                None => ci,
                Some(b) => {
                    if f.p.rd {
                        if self.rd_cost(mx, my, &ci, src, qp) < self.rd_cost(mx, my, &b, src, qp) { ci } else { b }
                    } else {
                        ci
                    }
                }
            });
        }
        let mut best = best.unwrap_or_else(|| self.analyse_intra(mx, my, qp, av, e, src, u64::MAX));
        // --- 16x8 / 8x16 partitions with a per-partition prediction direction
        if f.p.partitions && best.cost > (256 * 4) << f.p.depth.shift {
            for part in [Part::P16x8, Part::P8x16] {
                if let Some(c) = self.b_partition(mx, my, part, src, qp, lam, [res[0].0, res[1].0]) {
                    let better = if f.p.rd { self.rd_cost(mx, my, &c, src, qp) < self.rd_cost(mx, my, &best, src, qp) } else { c.cost < best.cost };
                    if better {
                        best = c;
                    }
                }
            }
        }
        let need_intra = f.p.always_intra || best.cost > (((256 * 3) << f.p.depth.shift) + 16 * lam as u64 / 256) * 3;
        if need_intra {
            let ic = self.analyse_intra(mx, my, qp, av, e, src, best.cost);
            if f.p.rd {
                if self.rd_cost(mx, my, &ic, src, qp) < self.rd_cost(mx, my, &best, src, qp) {
                    return ic;
                }
            } else if ic.cost < best.cost {
                return ic;
            }
        }
        best
    }
}

struct Src<P> {
    y: [P; 256],
    u: [P; 64],
    v: [P; 64],
}
