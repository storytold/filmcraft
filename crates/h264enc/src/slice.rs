//! Slice encoding: macroblock analysis, mode decision, residual coding, reconstruction and entropy coding.

use filmcraft_bitstream::BitWriter;

use crate::cabac::{BinSink, CabacEncoder, CabacEstimator, Contexts, CostTable};
use crate::dsp::{sad, sad16, satd, ssd};
use crate::intra::{Edge4, Edge8, filter8, mode_available, pred_chroma, pred4x4, pred8x8, pred16x16};
use crate::mbinfo::{MbInfo, MbKind, Mv, Part};
use crate::nal::{SliceHeader, SliceType, nal};
use crate::picture::{CHROMA_PAD, Frame, LUMA_PAD, RefPic, avg_into};
use crate::syntax::{MbCode, NbCtx};
use crate::tables::{BLK4_XY, NUM_CTX, RASTER_TO_BLK4, ZIGZAG4, ZIGZAG8, chroma_qp};
use crate::transform::{
    Deadzone, QuantTables, dequant_chroma_dc, dequant_luma_dc, dequant4, dequant8, fdct4, fdct8, idct4, idct8, quant_chroma_dc, quant_luma_dc, quant4, quant8,
};
use crate::{cabac_mb, cavlc};

mod b_slices;
mod intra;
mod macroblock;
mod motion;
mod residual;

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
}

/// Per-frame shared encoding state.
pub struct FrameEnc<'a> {
    pub p: &'a EncParams,
    pub qt: &'a QuantTables,
    pub cost_tab: &'a CostTable,
    pub st: SliceType,
    pub src: &'a Frame,
    pub l0: Option<&'a RefPic>,
    pub l1: Option<&'a RefPic>,
    pub qp: u8,
    pub aq: &'a [i8],
    pub hdr: SliceHeader,
    pub nal_ref_idc: u8,
    pub nal_type: u8,
    pub cabac_table: &'a [(i8, i8); NUM_CTX],
    pub lam: &'a Lambdas,
}

/// Lambda tables indexed by QP.
pub struct Lambdas {
    /// SATD/SAD-domain lambda, Q8.
    pub sad: [u32; 52],
    /// SSD-domain lambda (for RD), Q8 per 1/256 bit.
    pub ssd: [f64; 52],
}

impl Lambdas {
    pub fn new() -> Self {
        let mut sad = [0u32; 52];
        let mut ssd_ = [0f64; 52];
        for qp in 0..52 {
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
pub struct Band<'b> {
    pub y: &'b mut [u8],
    pub u: &'b mut [u8],
    pub v: &'b mut [u8],
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

struct MbEdges {
    top: [u8; 24],
    left: [u8; 16],
    tl: u8,
    ctop: [[u8; 8]; 2],
    cleft: [[u8; 8]; 2],
    ctl: [u8; 2],
}

#[derive(Clone)]
struct Cand {
    code: MbCode,
    info: MbInfo,
    ry: [u8; 256],
    ru: [u8; 64],
    rv: [u8; 64],
    cost: u64,
}

impl Cand {
    fn new() -> Box<Cand> {
        Box::new(Cand { code: MbCode::default(), info: MbInfo::default(), ry: [0; 256], ru: [0; 64], rv: [0; 64], cost: u64::MAX })
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

pub struct SliceEnc<'a, 'b> {
    f: &'a FrameEnc<'a>,
    band: Band<'b>,
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

impl<'a, 'b> SliceEnc<'a, 'b> {
    pub fn new(f: &'a FrameEnc<'a>, band: Band<'b>, mbs: &'b mut [MbInfo], y0: usize, y1: usize, slice_no: u16) -> Self {
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
            let ctx = Contexts::init(f.cabac_table, f.qp as i32);
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

    fn edges(&self, mx: usize, my: usize, av: Avail) -> MbEdges {
        let (px, py) = (mx * 16, my * 16);
        let mut e = MbEdges { top: [128; 24], left: [128; 16], tl: 128, ctop: [[128; 8]; 2], cleft: [[128; 8]; 2], ctl: [128; 2] };
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
    fn nb_part(&self, nb: MvNb, bx: i32, by: i32, list: usize) -> Option<(i8, Mv)> {
        let MvNb { cur, mask, mx, my } = nb;
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
    fn mvp(&self, nb: MvNb, geom: PartGeom, list: usize, rf: i8) -> Mv {
        let PartGeom { x, y, w, shape } = geom;
        let a = self.nb_part(nb, x - 1, y, list);
        let b = self.nb_part(nb, x, y - 1, list);
        let mut c = self.nb_part(nb, x + w, y - 1, list);
        if c.is_none() {
            c = self.nb_part(nb, x - 1, y - 1, list);
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
        let nb = MvNb { cur: &cur, mask: 0, mx, my };
        let a = self.nb_part(nb, -1, 0, 0);
        let b = self.nb_part(nb, 0, -1, 0);
        match (a, b) {
            (None, _) | (_, None) => Mv::ZERO,
            (Some((0, m)), _) if m == Mv::ZERO => Mv::ZERO,
            (_, Some((0, m))) if m == Mv::ZERO => Mv::ZERO,
            _ => self.mvp(nb, PartGeom { x: 0, y: 0, w: 4, shape: Shape::Normal }, 0, 0),
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
}

/// The macroblock (`mx`, `my`) being analysed: quantiser, neighbour availability and edge samples, and source.
#[derive(Clone, Copy)]
struct MbCtx<'x> {
    mx: usize,
    my: usize,
    qp: u8,
    av: Avail,
    e: &'x MbEdges,
    src: &'x Src,
}

/// Where a motion-vector-predictor neighbour lookup happens: the macroblock (`mx`, `my`) whose already
/// decided partitions (`mask` over the 4x4 blocks of `cur`) are visible.
#[derive(Clone, Copy)]
struct MvNb<'c> {
    cur: &'c MbInfo,
    mask: u16,
    mx: usize,
    my: usize,
}

/// A partition for motion-vector prediction: 4x4-block origin (`x`, `y`), width `w` (in 4x4 units), and shape.
#[derive(Clone, Copy)]
struct PartGeom {
    x: i32,
    y: i32,
    w: i32,
    shape: Shape,
}

/// A luma block to motion-search at picture position (`x`, `y`) against `src` (row stride `sstride`).
#[derive(Clone, Copy)]
struct SearchBlock<'s> {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    src: &'s [u8],
    sstride: usize,
}

/// Parameters of a sequential per-partition search of macroblock (`mx`, `my`) for one reference list.
#[derive(Clone, Copy)]
struct PartSearch {
    mx: usize,
    my: usize,
    part: Part,
    list: usize,
    lam: u32,
    seed: Mv,
}

/// Destination buffers (luma 16x16, chroma 8x8 each) of a motion-compensated prediction.
struct PredOut<'p> {
    y: &'p mut [u8; 256],
    u: &'p mut [u8; 64],
    v: &'p mut [u8; 64],
}

struct Src {
    y: [u8; 256],
    u: [u8; 64],
    v: [u8; 64],
}
