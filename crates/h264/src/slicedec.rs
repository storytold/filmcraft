//! Slice data decoding: macroblock parsing (CAVLC), motion vector derivation, and reconstruction.

use crate::cavlc;
use crate::deblock;
use crate::error::{Result, ensure, invalid};
use crate::inter::{self, Weight};
use crate::intra::{self, Avail};
use crate::mbtypes::*;
use crate::params::{Pps, Sps};
use crate::picture::*;
use crate::slice::{SliceHeader, SliceType};
use crate::tables::*;
use crate::transform::{self, LevelScale};
use filmcraft_bitstream::BitReader;

/// Per-slice data kept with the picture (deblocking, motion field export).
#[derive(Clone, Debug, Default)]
pub struct SliceInfo {
    pub disable_deblocking_filter_idc: u32,
    pub alpha_offset: i32,
    pub beta_offset: i32,
    /// Unique ids of the pictures in RefPicList0/1.
    pub ref_ids: [Vec<u32>; 2],
}

/// Whether macroblock-level parse tracing is on (`H264_TRACE=1`): each decoded macroblock's syntax
/// is printed to stderr. A debugging aid for bitstream desyncs; off unless the variable is set.
pub fn trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("H264_TRACE").is_some())
}

/// Apply `mb_qp_delta` to a `QPY` (equation 7-10), wrapping over the `52 + QpBdOffsetY` values of
/// 7-4. Both the input and the result are `QPY` in `-qpb..=51`.
///
/// The delta is normally small, but 7-4 only constrains the *result* (`QPY` stays inside
/// `-QpBdOffsetY..=51`): with a deep `QpBdOffsetY` a single delta may legally exceed the 8-bit
/// `-26..=25` bound, and the wrap makes the move land back inside the range.
pub fn wrap_qp(qp: i32, delta: i32, qpb: i32) -> i32 {
    (qp + delta + 52 + 2 * qpb) % (52 + qpb) - qpb
}

/// The picture under construction: private reconstruction buffers plus the shared [`Frame`] that
/// finished (deblocked) macroblock rows are published into.
pub struct PicState {
    pub planes: Planes,
    pub mbs: Vec<MbState>,
    pub slices: Vec<SliceInfo>,
    pub mb_w: usize,
    pub mb_h: usize,
    pub poc: i32,
    pub decoded_mbs: usize,
    pub frame: FrameRef,
    /// Decoded macroblocks per MB row.
    row_count: Vec<u16>,
    /// Leading MB rows that are fully reconstructed / deblocked / published.
    rows_recon: usize,
    rows_deblocked: usize,
    rows_published: usize,
    /// Draft mode for a non-reference picture: rows are published without deblocking.
    pub skip_deblock: bool,
}

impl PicState {
    pub fn new(frame: FrameRef) -> Self {
        let (mb_w, mb_h) = (frame.mb_w, frame.mb_h());
        PicState {
            planes: Planes::new(mb_w * 16, mb_h * 16, frame.fmt),
            mbs: vec![MbState::default(); mb_w * mb_h],
            slices: Vec::new(),
            mb_w,
            mb_h,
            poc: frame.poc,
            decoded_mbs: 0,
            frame,
            row_count: vec![0; mb_h],
            rows_recon: 0,
            rows_deblocked: 0,
            rows_published: 0,
            skip_deblock: false,
        }
    }

    /// Reuse the buffers of a finished picture of the same size for a new frame.
    pub fn reset(&mut self, frame: FrameRef) {
        debug_assert_eq!((self.mb_w, self.mb_h), (frame.mb_w, frame.mb_h()));
        // start_mb() re-initialises each macroblock before use; only availability must be reset
        for m in &mut self.mbs {
            m.slice_num = u32::MAX;
        }
        self.slices.clear();
        self.poc = frame.poc;
        self.decoded_mbs = 0;
        self.frame = frame;
        self.row_count.fill(0);
        self.rows_recon = 0;
        self.rows_deblocked = 0;
        self.rows_published = 0;
        self.skip_deblock = false;
    }

    /// Record a decoded macroblock and deblock / publish rows that became final.
    #[inline]
    pub fn mb_done(&mut self, addr: usize) {
        self.decoded_mbs += 1;
        let r = addr / self.mb_w;
        self.row_count[r] += 1;
        if self.row_count[r] as usize >= self.mb_w && r == self.rows_recon {
            while self.rows_recon < self.mb_h && self.row_count[self.rows_recon] as usize >= self.mb_w {
                self.rows_recon += 1;
            }
            self.flush_rows(false);
        }
    }

    /// Deblock and publish rows. With `finished`, everything remaining is processed (also rows with
    /// missing macroblocks, so that waiting readers never block forever).
    pub fn flush_rows(&mut self, finished: bool) {
        // Deblocking row r must wait until row r + 1 is reconstructed (its intra prediction reads the
        // unfiltered bottom line of row r); row r is final once row r + 1 is deblocked.
        let deblock_limit = if finished { self.mb_h } else { self.rows_recon.saturating_sub(1) };
        while self.rows_deblocked < deblock_limit {
            let r = self.rows_deblocked;
            if !self.skip_deblock {
                let fmt = self.planes.fmt;
                for addr in r * self.mb_w..(r + 1) * self.mb_w {
                    deblock::deblock_mb(self, addr, self.mb_w, fmt);
                }
            }
            self.rows_deblocked += 1;
        }
        let publish_limit = if finished { self.mb_h } else { self.rows_deblocked.saturating_sub(1) };
        while self.rows_published < publish_limit {
            let r = self.rows_published;
            let slices = &self.slices;
            let ids =
                |st: &MbState, l: usize, ri: i8| slices.get(st.slice_num as usize).and_then(|s| s.ref_ids[l].get(ri as usize).copied()).unwrap_or(u32::MAX);
            let row = Frame::make_row(&self.planes, r, &self.mbs, &ids);
            self.frame.publish(r, row);
            self.rows_published += 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WpMode {
    Default,
    Explicit,
    Implicit,
}

/// Prediction weight tables of a slice.
struct Weights {
    mode: WpMode,
    luma_denom: i32,
    chroma_denom: i32,
    /// [list][ref] -> (weight, offset) for luma and two chroma components.
    luma: [Vec<(i32, i32)>; 2],
    chroma: [Vec<[(i32, i32); 2]>; 2],
    /// Implicit weights (w0, w1) indexed by ref0 * n1 + ref1.
    implicit: Vec<(i32, i32)>,
    n1: usize,
}

/// Current macroblock syntax (prediction part).
#[derive(Clone, Copy, Default)]
pub(crate) struct MbCur {
    pub info: MbTypeInfo,
    pub sub: [SubMbInfo; 4],
    /// rem_intra_pred_mode per block, or -1 when prev_intra_pred_mode_flag was set.
    pub rem_mode: [i8; 16],
    pub cbp: u8,
    pub ref_idx: [[i8; 4]; 2],
    /// mvd per (partition * 4 + sub-partition).
    pub mvd: [[[i16; 2]; 16]; 2],
}

/// Coefficient scratch for one macroblock.
pub(crate) struct Scratch {
    /// Luma: 16 4x4 blocks (raster block index * 16 + raster coefficient) or 4 8x8 blocks (b8 * 64 + raster).
    pub coef: [i32; 256],
    /// Chroma: [Cb, Cr][raster block * 16 + raster coefficient]; 2x2 blocks in 4:2:0, 2x4 in 4:2:2.
    pub coef_c: [[i32; 128]; 2],
    /// Luma blocks with coefficients: raster 4x4 bits, or b8 bits in 8x8 mode.
    pub blk_nz: u16,
    /// Chroma blocks with coefficients (bit per raster block, up to 8 in 4:2:2).
    pub c_nz: [u8; 2],
    pub pred: [[u16; 256]; 2],
    /// Chroma prediction scratch, 8 wide x 8 high (4:2:0) or 8 x 16 (4:2:2), stride 8.
    pub pred_c: [[[u16; 128]; 2]; 2],
}

impl Scratch {
    pub fn new() -> Box<Self> {
        Box::new(Scratch { coef: [0; 256], coef_c: [[0; 128]; 2], blk_nz: 0, c_nz: [0; 2], pred: [[0; 256]; 2], pred_c: [[[0; 128]; 2]; 2] })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Nb {
    pub avail: bool,
    pub ref_idx: i8,
    pub mv: [i16; 2],
}

const NB_NA: Nb = Nb { avail: false, ref_idx: -1, mv: [0, 0] };

/// Parse I_PCM samples (7.3.5): 256 luma `u(BitDepthY)` then `2 * MbWidthC * MbHeightC` chroma
/// `u(BitDepthC)` (Cb raster then Cr), starting at bit `bit_pos` of `data`. Returns the samples
/// ([256 luma][Cb][Cr]) and the bit position just after them.
pub fn read_pcm_samples(data: &[u8], bit_pos: usize, fmt: Format) -> Result<(Vec<u16>, usize)> {
    let n = 8 * fmt.chroma_row_lines(); // MbWidthC * MbHeightC per chroma component
    let total = 256 + 2 * n;
    let bd_y = fmt.bit_depth.min(16);
    let bd_c = fmt.bit_depth_c.min(16);
    let end = bit_pos.saturating_add(256 * bd_y as usize).saturating_add(2 * n * bd_c as usize);
    ensure!(data.len().saturating_mul(8) >= end, "truncated I_PCM macroblock");
    let mut r = BitReader::new(data);
    r.seek_bits(bit_pos);
    let mut out = Vec::with_capacity(total);
    for _ in 0..256 {
        out.push(r.read_bits(bd_y)? as u16);
    }
    for _ in 0..2 * n {
        out.push(r.read_bits(bd_c)? as u16);
    }
    Ok((out, end))
}

pub struct SliceDecoder<'a> {
    pub sh: &'a SliceHeader,
    pub pps: &'a Pps,
    pub sps: &'a Sps,
    pub pic: &'a mut PicState,
    pub refs: &'a [Vec<RefPic>; 2],
    pub ls: &'a LevelScale,
    pub slice_num: u32,
    pub mb_w: usize,
    pub mb_h: usize,
    pub qp: i32,
    pub chroma_qp_offset: [i32; 2],
    // current macroblock
    pub mb_addr: usize,
    pub mb_x: usize,
    pub mb_y: usize,
    /// Neighbouring macroblocks A (left), B (above), C (above right), D (above left) when available.
    pub nb: [Option<usize>; 4],
    pub mv_done: u16,
    pub cur: MbCur,
    pub s: Box<Scratch>,
    weights: Weights,
    /// Temporal direct: DistScaleFactor per refIdxL0, and whether pic0 is long-term.
    tdirect: Vec<(i32, bool)>,
    /// CABAC: previous MB in decoding order had a non-zero mb_qp_delta.
    pub prev_qp_delta_nz: bool,
}

/// Interpolate a bw x bh luma block at (px, py) with motion vector `mv` from reference `f` into `y`,
/// and the corresponding chroma blocks into `c` (stride `cs`). Chroma geometry and the chroma
/// motion-vector scaling follow the stream's ChromaArrayType (8.4.1.4 / 8.4.2.2).
#[allow(clippy::too_many_arguments)]
#[inline]
fn predict_block(f: &Frame, mv: [i16; 2], px: usize, py: usize, bw: usize, bh: usize, y: (&mut [u16], usize), c: [&mut [u16]; 2], cs: usize) {
    let fmt = f.fmt;
    const LW: usize = 24;
    let mut win = [0u16; LW * 21];
    let ix = px as i32 + (mv[0] as i32 >> 2);
    let iy = py as i32 + (mv[1] as i32 >> 2);
    f.luma_window(ix - 2, iy - 2, bw + 5, bh + 5, &mut win, LW);
    inter::mc_luma_win(&win, LW, (mv[0] & 3) as u32, (mv[1] & 3) as u32, bw, bh, y.0, y.1, fmt.max_y());
    let (cw, ch) = (bw / 2, bh >> fmt.chroma_y_shift);
    // 4:2:0 halves the vertical position and uses eighth-sample fractions (8-227..8-230);
    // 4:2:2 keeps the luma motion vector (8-221/8-222) with quarter-sample vertical positions
    // (8-231..8-234), derived by `chroma_frac422`; the interpolation kernels take the raw
    // `mv & 7` fractions and derive the chroma fraction themselves.
    let (cx, cy, fx, fy) = if fmt.chroma_y_shift == 0 {
        inter::chroma_frac422(px as i32, py as i32, [mv[0] as i32, mv[1] as i32])
    } else {
        ((px / 2) as i32 + (mv[0] as i32 >> 3), (py / 2) as i32 + (mv[1] as i32 >> 3), (mv[0] & 7) as u32, (mv[1] & 7) as u32)
    };
    for (comp, out) in c.into_iter().enumerate() {
        let mut w = [0u16; 16 * 17];
        f.chroma_window(comp, cx, cy, cw + 1, ch + 1, &mut w, 16);
        inter::mc_chroma_win(&w, 16, fx, fy, cw, ch, out, cs, fmt.max_c(), fmt.chroma_y_shift == 0);
    }
}

#[inline(always)]
fn median(a: i16, b: i16, c: i16) -> i16 {
    a.max(b).min(a.min(b).max(c))
}

impl<'a> SliceDecoder<'a> {
    pub fn new(sh: &'a SliceHeader, pps: &'a Pps, sps: &'a Sps, pic: &'a mut PicState, refs: &'a [Vec<RefPic>; 2], ls: &'a LevelScale) -> Result<Self> {
        let slice_num = pic.slices.len() as u32;
        let chroma_qp_offset = [pps.chroma_qp_index_offset, pps.second_chroma_qp_index_offset];
        pic.slices.push(SliceInfo {
            disable_deblocking_filter_idc: sh.disable_deblocking_filter_idc,
            alpha_offset: sh.slice_alpha_c0_offset_div2 * 2,
            beta_offset: sh.slice_beta_offset_div2 * 2,
            ref_ids: [refs[0].iter().map(|r| r.id()).collect(), refs[1].iter().map(|r| r.id()).collect()],
        });
        let weights = Self::build_weights(sh, pps, refs, pic.poc, pic.planes.fmt);
        let mut tdirect = Vec::new();
        if sh.slice_type == SliceType::B && !sh.direct_spatial_mv_pred && !refs[1].is_empty() {
            let poc1 = refs[1][0].poc();
            for r0 in refs[0].iter() {
                let tb = (pic.poc - r0.poc()).clamp(-128, 127);
                let td = (poc1 - r0.poc()).clamp(-128, 127);
                if td == 0 || r0.long_term {
                    tdirect.push((256, true));
                } else {
                    let tx = (16384 + (td / 2).abs()) / td;
                    tdirect.push((((tb * tx + 32) >> 6).clamp(-1024, 1023), false));
                }
            }
        }
        let (mb_w, mb_h) = (pic.mb_w, pic.mb_h);
        Ok(SliceDecoder {
            sh,
            pps,
            sps,
            pic,
            refs,
            ls,
            slice_num,
            mb_w,
            mb_h,
            qp: sh.qp(pps),
            chroma_qp_offset,
            mb_addr: 0,
            mb_x: 0,
            mb_y: 0,
            nb: [None; 4],
            mv_done: 0,
            cur: MbCur::default(),
            s: Scratch::new(),
            weights,
            tdirect,
            prev_qp_delta_nz: false,
        })
    }

    /// Prediction weights of the slice (8.4.3): explicit offsets are scaled to the sample depth
    /// (`o0C = luma_offset * (1 << (BitDepthY - 8))`, 8-291/8-292/8-296/8-297); weights are not.
    fn build_weights(sh: &SliceHeader, pps: &Pps, refs: &[Vec<RefPic>; 2], cur_poc: i32, fmt: Format) -> Weights {
        let mut w = Weights {
            mode: WpMode::Default,
            luma_denom: 0,
            chroma_denom: 0,
            luma: [Vec::new(), Vec::new()],
            chroma: [Vec::new(), Vec::new()],
            implicit: Vec::new(),
            n1: refs[1].len(),
        };
        let explicit = (pps.weighted_pred && sh.slice_type.is_p()) || (pps.weighted_bipred_idc == 1 && sh.slice_type.is_b());
        if explicit {
            if let Some(t) = &sh.pred_weight_table {
                let yoff = 1i32 << fmt.bit_depth.saturating_sub(8).min(8);
                let coff = 1i32 << fmt.bit_depth_c.saturating_sub(8).min(8);
                w.mode = WpMode::Explicit;
                w.luma_denom = t.luma_log2_denom as i32;
                w.chroma_denom = t.chroma_log2_denom as i32;
                for (l, entries) in [&t.l0, &t.l1].into_iter().enumerate() {
                    w.luma[l] = entries.iter().map(|e| (e.luma_weight, e.luma_offset * yoff)).collect();
                    w.chroma[l] =
                        entries.iter().map(|e| [(e.chroma_weight[0], e.chroma_offset[0] * coff), (e.chroma_weight[1], e.chroma_offset[1] * coff)]).collect();
                }
            }
        } else if pps.weighted_bipred_idc == 2 && sh.slice_type.is_b() {
            w.mode = WpMode::Implicit;
            for r0 in refs[0].iter() {
                for r1 in refs[1].iter() {
                    let tb = (cur_poc - r0.poc()).clamp(-128, 127);
                    let td = (r1.poc() - r0.poc()).clamp(-128, 127);
                    let pair = if td == 0 || r0.long_term || r1.long_term {
                        (32, 32)
                    } else {
                        let tx = (16384 + (td / 2).abs()) / td;
                        let dsf = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
                        if (dsf >> 2) < -64 || (dsf >> 2) > 128 { (32, 32) } else { (64 - (dsf >> 2), dsf >> 2) }
                    };
                    w.implicit.push(pair);
                }
            }
        }
        w
    }

    // ------------------------------------------------------------------------------------------
    // Neighbour helpers
    // ------------------------------------------------------------------------------------------

    /// Set up the current macroblock address and neighbour availability.
    pub fn start_mb(&mut self, addr: usize) {
        self.mb_addr = addr;
        self.mb_x = addr % self.mb_w;
        self.mb_y = addr / self.mb_w;
        let sn = self.slice_num;
        let mbs = &self.pic.mbs;
        let w = self.mb_w;
        let (x, y) = (self.mb_x, self.mb_y);
        let ok = |a: usize| if mbs[a].slice_num == sn { Some(a) } else { None };
        self.nb = [
            if x > 0 { ok(addr - 1) } else { None },
            if y > 0 { ok(addr - w) } else { None },
            if y > 0 && x + 1 < w { ok(addr - w + 1) } else { None },
            if y > 0 && x > 0 { ok(addr - w - 1) } else { None },
        ];
        self.mv_done = 0;
        self.pic.mbs[addr] = MbState { slice_num: sn, ..MbState::default() };
    }

    #[inline]
    pub fn mb(&self) -> &MbState {
        &self.pic.mbs[self.mb_addr]
    }
    #[inline]
    pub fn mb_mut(&mut self) -> &mut MbState {
        &mut self.pic.mbs[self.mb_addr]
    }

    /// Neighbour MB usable for intra prediction (constrained_intra_pred aware).
    fn intra_nb(&self, n: usize) -> bool {
        match self.nb[n] {
            None => false,
            Some(a) => !self.pps.constrained_intra_pred || self.pic.mbs[a].kind.is_intra(),
        }
    }

    /// Effective chroma QP (`QP′C = QpC + QpBdOffsetC`, 8.5.8 / 8-309..8-312) of QPY `qp`.
    fn chroma_qp(&self, qp: i32, c: usize) -> i32 {
        let off = self.sps.qp_bd_offset_c();
        let qpi = (qp + self.chroma_qp_offset[c]).clamp(-off, 51);
        let qpc = if qpi < 30 { qpi } else { QPC_TABLE[qpi as usize] as i32 };
        qpc + off
    }

    /// nC for a luma 4x4 block (raster index) - 9.2.1.
    fn nc_luma(&self, raster: usize) -> i32 {
        let (bx, by) = (raster & 3, raster >> 2);
        let cur = self.mb();
        let a = if bx > 0 { Some(cur.nnz[raster - 1]) } else { self.nb[0].map(|m| self.pic.mbs[m].nnz[by * 4 + 3]) };
        let b = if by > 0 { Some(cur.nnz[raster - 4]) } else { self.nb[1].map(|m| self.pic.mbs[m].nnz[12 + bx]) };
        match (a, b) {
            (Some(a), Some(b)) => (a as i32 + b as i32 + 1) >> 1,
            (Some(a), None) => a as i32,
            (None, Some(b)) => b as i32,
            _ => 0,
        }
    }

    fn nc_chroma(&self, c: usize, raster: usize) -> i32 {
        let (bx, by) = (raster & 1, raster >> 1);
        let cur = self.mb();
        // bottom block row of the MB above (1 in 4:2:0, 2 in 4:2:2; raster base 2 / 6)
        let btm = ((2usize << (1 - self.pic.planes.fmt.chroma_y_shift as usize)) - 1) * 2;
        let a = if bx > 0 { Some(cur.nnz_c[c][raster - 1]) } else { self.nb[0].map(|m| self.pic.mbs[m].nnz_c[c][by * 2 + 1]) };
        let b = if by > 0 { Some(cur.nnz_c[c][raster - 2]) } else { self.nb[1].map(|m| self.pic.mbs[m].nnz_c[c][btm + bx]) };
        match (a, b) {
            (Some(a), Some(b)) => (a as i32 + b as i32 + 1) >> 1,
            (Some(a), None) => a as i32,
            (None, Some(b)) => b as i32,
            _ => 0,
        }
    }

    /// Motion data of the neighbouring 4x4 block at (x, y) (4x4 units, relative to the current MB).
    pub(crate) fn nb_motion(&self, list: usize, x: i32, y: i32) -> Nb {
        let (mb, bx, by) = if x < 0 {
            if y < 0 {
                (self.nb[3], 3, 3)
            } else if y > 3 {
                (None, 0, 0)
            } else {
                (self.nb[0], 3, y as usize)
            }
        } else if x > 3 {
            if y < 0 { (self.nb[2], 0, 3) } else { (None, 0, 0) }
        } else if y < 0 {
            (self.nb[1], x as usize, 3)
        } else {
            let r = (y * 4 + x) as usize;
            if self.mv_done & (1 << r) == 0 {
                return NB_NA;
            }
            (Some(self.mb_addr), x as usize, y as usize)
        };
        let Some(m) = mb else { return NB_NA };
        let st = &self.pic.mbs[m];
        let b8 = (by >> 1) * 2 + (bx >> 1);
        let r = st.ref_idx[list][b8];
        if r < 0 {
            return Nb { avail: true, ref_idx: -1, mv: [0, 0] };
        }
        Nb { avail: true, ref_idx: r, mv: st.mv[list][by * 4 + bx] }
    }

    /// Returns (A, B, C) neighbour motion for a partition at (x, y) of width w (4x4 units), with C
    /// replaced by D when unavailable.
    fn nb_abc(&self, list: usize, x: usize, y: usize, w: usize) -> (Nb, Nb, Nb) {
        let (x, y, w) = (x as i32, y as i32, w as i32);
        let a = self.nb_motion(list, x - 1, y);
        let b = self.nb_motion(list, x, y - 1);
        let mut c = self.nb_motion(list, x + w, y - 1);
        if !c.avail {
            c = self.nb_motion(list, x - 1, y - 1);
        }
        (a, b, c)
    }

    /// Luma motion vector prediction (8.4.1.3). `shape`: 1 = 16x8, 2 = 8x16, 0 = other.
    #[allow(clippy::too_many_arguments)]
    fn mvp(&self, list: usize, x: usize, y: usize, w: usize, ref_idx: i8, shape: u8, part: usize) -> [i16; 2] {
        let (a, b, c) = self.nb_abc(list, x, y, w);
        if shape == 1 {
            if part == 0 && b.ref_idx == ref_idx {
                return b.mv;
            }
            if part == 1 && a.ref_idx == ref_idx {
                return a.mv;
            }
        } else if shape == 2 {
            if part == 0 && a.ref_idx == ref_idx {
                return a.mv;
            }
            if part == 1 && c.ref_idx == ref_idx {
                return c.mv;
            }
        }
        Self::median_pred(a, b, c, ref_idx)
    }

    fn median_pred(a: Nb, mut b: Nb, mut c: Nb, ref_idx: i8) -> [i16; 2] {
        if !b.avail && !c.avail && a.avail {
            b = a;
            c = a;
        }
        let ma = a.ref_idx == ref_idx;
        let mb = b.ref_idx == ref_idx;
        let mc = c.ref_idx == ref_idx;
        match (ma, mb, mc) {
            (true, false, false) => a.mv,
            (false, true, false) => b.mv,
            (false, false, true) => c.mv,
            _ => [median(a.mv[0], b.mv[0], c.mv[0]), median(a.mv[1], b.mv[1], c.mv[1])],
        }
    }

    /// Store motion for a rectangle (4x4 units) of the current MB and mark it decoded.
    fn set_motion(&mut self, x: usize, y: usize, w: usize, h: usize, refs: [i8; 2], mvs: [[i16; 2]; 2]) {
        let st = &mut self.pic.mbs[self.mb_addr];
        for yy in y..y + h {
            for xx in x..x + w {
                let r = yy * 4 + xx;
                for l in 0..2 {
                    st.mv[l][r] = if refs[l] >= 0 { mvs[l] } else { [0, 0] };
                }
                self.mv_done |= 1 << r;
            }
        }
        for l in 0..2 {
            // ref_idx is per 8x8; rectangles never straddle 8x8 blocks with different refs
            for yy in (y..y + h).step_by(1) {
                for xx in x..x + w {
                    st.ref_idx[l][(yy >> 1) * 2 + (xx >> 1)] = refs[l];
                }
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // Direct prediction (8.4.1.2)
    // ------------------------------------------------------------------------------------------

    /// Co-located (mvCol, refIdxCol, referenced picture id) for 4x4 block `raster` of the current MB.
    fn colocated(&self, raster: usize) -> ([i16; 2], i8, u32) {
        let Some(col) = self.refs[1].first() else { return ([0, 0], -1, u32::MAX) };
        let raster = if self.sps.direct_8x8_inference {
            let (bx, by) = (raster & 3, raster >> 2);
            (if by >= 2 { 12 } else { 0 }) + if bx >= 2 { 3 } else { 0 }
        } else {
            raster
        };
        if col.frame.mb_w != self.mb_w || self.mb_y >= col.frame.mb_h() {
            return ([0, 0], -1, u32::MAX);
        }
        let m = col.frame.row(self.mb_y);
        let mb = self.mb_x;
        if m.intra[mb] {
            return ([0, 0], -1, u32::MAX);
        }
        let b8 = mb * 4 + (raster >> 3) * 2 + ((raster & 3) >> 1);
        let l = if m.ref_idx[0][b8] >= 0 { 0 } else { 1 };
        (m.mv[l][mb * 16 + raster], m.ref_idx[l][b8], m.ref_id[l][b8])
    }

    /// Spatial direct: MB-level reference indices, predicted mvs and directZeroPrediction (8.4.1.2.2).
    fn spatial_direct_base(&self) -> ([i8; 2], [[i16; 2]; 2], bool) {
        let mut refs = [-1i8; 2];
        let mut nbs = [(NB_NA, NB_NA, NB_NA); 2];
        let min_pos = |x: i8, y: i8| if x >= 0 && y >= 0 { x.min(y) } else { x.max(y) };
        for (l, nb) in nbs.iter_mut().enumerate() {
            *nb = self.nb_abc(l, 0, 0, 4);
            refs[l] = min_pos(nb.0.ref_idx, min_pos(nb.1.ref_idx, nb.2.ref_idx));
        }
        let mut mvs = [[0i16; 2]; 2];
        if refs[0] < 0 && refs[1] < 0 {
            return ([0, 0], mvs, true);
        }
        for l in 0..2 {
            if refs[l] >= 0 {
                let (a, b, c) = nbs[l];
                mvs[l] = Self::median_pred(a, b, c, refs[l]);
            }
        }
        (refs, mvs, false)
    }

    /// Derive direct-mode motion for the 8x8 blocks in `b8_mask` of the current MB.
    fn derive_direct(&mut self, b8_mask: u8) -> Result<()> {
        let spatial = self.sh.direct_spatial_mv_pred;
        let (base_refs, base_mvs, direct_zero) = if spatial { self.spatial_direct_base() } else { ([0, 0], [[0, 0]; 2], false) };
        let col_short_term = self.refs[1].first().map(|r| !r.long_term).unwrap_or(false);
        for b8 in 0..4 {
            if b8_mask & (1 << b8) == 0 {
                continue;
            }
            let (x8, y8) = ((b8 & 1) * 2, (b8 >> 1) * 2);
            let unit = if self.sps.direct_8x8_inference { 2 } else { 1 };
            for sub in (0..4).filter(|s| unit == 1 || *s == 0) {
                let (x, y) = (x8 + (sub & 1), y8 + (sub >> 1));
                let raster = y * 4 + x;
                let (mv_col, ref_col, id_col) = self.colocated(raster);
                let (refs, mvs) = if spatial {
                    let col_zero = col_short_term && ref_col == 0 && (-1..=1).contains(&mv_col[0]) && (-1..=1).contains(&mv_col[1]);
                    let mut mvs = base_mvs;
                    for l in 0..2 {
                        if direct_zero || base_refs[l] < 0 || (base_refs[l] == 0 && col_zero) {
                            mvs[l] = [0, 0];
                        }
                    }
                    (base_refs, mvs)
                } else {
                    let ref0 = if ref_col < 0 {
                        0
                    } else {
                        match self.refs[0].iter().position(|r| r.id() == id_col) {
                            Some(i) => i as i8,
                            None => 0,
                        }
                    };
                    let (dsf, ltr) = self.tdirect.get(ref0 as usize).copied().unwrap_or((256, true));
                    let (mv0, mv1) = if ltr {
                        (mv_col, [0, 0])
                    } else {
                        let s = |c: i16| ((dsf * c as i32 + 128) >> 8) as i16;
                        let m0 = [s(mv_col[0]), s(mv_col[1])];
                        (m0, [m0[0] - mv_col[0], m0[1] - mv_col[1]])
                    };
                    ([ref0, 0], [mv0, mv1])
                };
                self.set_motion(x, y, unit, unit, refs, mvs);
            }
        }
        let st = &mut self.pic.mbs[self.mb_addr];
        st.direct8x8 |= b8_mask;
        Ok(())
    }

    // ------------------------------------------------------------------------------------------
    // Motion vector derivation for a parsed inter macroblock
    // ------------------------------------------------------------------------------------------

    /// P_Skip motion (8.4.1.1).
    pub fn derive_p_skip(&mut self) {
        let a = self.nb_motion(0, -1, 0);
        let b = self.nb_motion(0, 0, -1);
        let mv = if self.nb[0].is_none() || self.nb[1].is_none() || (a.ref_idx == 0 && a.mv == [0, 0]) || (b.ref_idx == 0 && b.mv == [0, 0]) {
            [0, 0]
        } else {
            self.mvp(0, 0, 0, 4, 0, 0, 0)
        };
        self.set_motion(0, 0, 4, 4, [0, -1], [mv, [0, 0]]);
    }

    /// Derive mvs of the current (non-skip) inter MB from `self.cur` (ref_idx, mvd).
    pub fn derive_inter_motion(&mut self) -> Result<()> {
        let info = self.cur.info;
        if info.kind == MbKind::BDirect16x16 || info.kind == MbKind::BSkip {
            return self.derive_direct(0xf);
        }
        let n = [self.refs[0].len(), self.refs[1].len()];
        if info.part != Part::P8x8 {
            let shape = match info.part {
                Part::P16x8 => 1,
                Part::P8x16 => 2,
                _ => 0,
            };
            for p in 0..info.part.num_parts() {
                let (x, y, w, h) = info.part.rect(p);
                let mut refs = [-1i8; 2];
                let mut mvs = [[0i16; 2]; 2];
                for l in 0..2 {
                    if info.pred[p] & (1 << l) != 0 {
                        let r = self.cur.ref_idx[l][p];
                        ensure!((r as usize) < n[l], "ref_idx {r} out of range (list {l}, size {})", n[l]);
                        let mvp = self.mvp(l, x, y, w, r, shape, p);
                        let d = self.cur.mvd[l][p * 4];
                        refs[l] = r;
                        mvs[l] = [mvp[0].wrapping_add(d[0]), mvp[1].wrapping_add(d[1])];
                    }
                }
                self.set_motion(x, y, w, h, refs, mvs);
            }
            return Ok(());
        }
        for p in 0..4 {
            let sub = self.cur.sub[p];
            let (x8, y8) = ((p & 1) * 2, (p >> 1) * 2);
            if sub.direct {
                self.derive_direct(1 << p)?;
                continue;
            }
            let mut refs = [-1i8; 2];
            for (l, r) in refs.iter_mut().enumerate() {
                if sub.pred & (1 << l) != 0 {
                    let v = self.cur.ref_idx[l][p];
                    ensure!((v as usize) < n[l], "ref_idx {v} out of range (list {l}, size {})", n[l]);
                    *r = v;
                }
            }
            for j in 0..sub.shape.num_parts() {
                let (sx, sy, w, h) = sub.shape.rect(j);
                let (x, y) = (x8 + sx, y8 + sy);
                let mut mvs = [[0i16; 2]; 2];
                for l in 0..2 {
                    if refs[l] >= 0 {
                        let mvp = self.mvp(l, x, y, w, refs[l], 0, 0);
                        let d = self.cur.mvd[l][p * 4 + j];
                        mvs[l] = [mvp[0].wrapping_add(d[0]), mvp[1].wrapping_add(d[1])];
                    }
                }
                self.set_motion(x, y, w, h, refs, mvs);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------------------------------
    // Inter prediction
    // ------------------------------------------------------------------------------------------

    fn weight_for(&self, refs: [i8; 2], comp: usize) -> Weight {
        let w = &self.weights;
        match w.mode {
            WpMode::Default => Weight::Default,
            WpMode::Implicit => {
                if refs[0] >= 0 && refs[1] >= 0 {
                    let (w0, w1) = w.implicit[refs[0] as usize * w.n1 + refs[1] as usize];
                    Weight::Weighted { log_wd: 5, w0, w1, o0: 0, o1: 0 }
                } else {
                    Weight::Default
                }
            }
            WpMode::Explicit => {
                let get = |l: usize| -> (i32, i32) {
                    if refs[l] < 0 {
                        return (0, 0);
                    }
                    let r = refs[l] as usize;
                    if comp == 0 {
                        w.luma[l].get(r).copied().unwrap_or((1 << w.luma_denom, 0))
                    } else {
                        w.chroma[l].get(r).map(|c| c[comp - 1]).unwrap_or((1 << w.chroma_denom, 0))
                    }
                };
                let (w0, o0) = get(0);
                let (w1, o1) = get(1);
                let log_wd = if comp == 0 { w.luma_denom } else { w.chroma_denom };
                Weight::Weighted { log_wd, w0, w1, o0, o1 }
            }
        }
    }

    /// Motion-compensated prediction of a rectangle (4x4 units) written into the picture.
    fn mc_rect(&mut self, x4: usize, y4: usize, w4: usize, h4: usize) {
        let st = &self.pic.mbs[self.mb_addr];
        let r = y4 * 4 + x4;
        let b8 = (y4 >> 1) * 2 + (x4 >> 1);
        let refs = [st.ref_idx[0][b8], st.ref_idx[1][b8]];
        let mvs = [st.mv[0][r], st.mv[1][r]];
        let (bw, bh) = (w4 * 4, h4 * 4);
        let wts = [self.weight_for(refs, 0), self.weight_for(refs, 1), self.weight_for(refs, 2)];
        let px = self.mb_x * 16 + x4 * 4;
        let py = self.mb_y * 16 + y4 * 4;
        let use0 = refs[0] >= 0 && self.refs[0].get(refs[0] as usize).is_some();
        let use1 = refs[1] >= 0 && self.refs[1].get(refs[1] as usize).is_some();
        let ys = self.pic.planes.width;
        let cs = self.pic.planes.cwidth;
        let fmt = self.pic.planes.fmt;
        let (cx, cy) = (px / 2, py >> fmt.chroma_y_shift);
        if use0 != use1 && wts.iter().all(|w| *w == Weight::Default) {
            // single-list unweighted prediction: interpolate straight into the picture
            let l = if use0 { 0 } else { 1 };
            let f = &self.refs[l][refs[l] as usize].frame;
            let planes = &mut self.pic.planes;
            predict_block(f, mvs[l], px, py, bw, bh, (&mut planes.y[py * ys + px..], ys), [&mut planes.cb[cy * cs + cx..], &mut planes.cr[cy * cs + cx..]], cs);
            return;
        }
        let s = &mut *self.s;
        for l in 0..2 {
            if refs[l] < 0 {
                continue;
            }
            let Some(rp) = self.refs[l].get(refs[l] as usize) else { continue };
            let [pc0, pc1] = &mut s.pred_c[l];
            predict_block(&rp.frame, mvs[l], px, py, bw, bh, (&mut s.pred[l][..], 16), [&mut pc0[..], &mut pc1[..]], 8);
        }
        inter::weighted_store(
            &mut self.pic.planes.y[py * ys + px..],
            ys,
            if use0 { Some(&s.pred[0][..]) } else { None },
            if use1 { Some(&s.pred[1][..]) } else { None },
            16,
            bw,
            bh,
            wts[0],
            fmt.max_y(),
        );
        for c in 0..2 {
            let wc = wts[c + 1];
            let a = if use0 { Some(&s.pred_c[0][c][..]) } else { None };
            let b = if use1 { Some(&s.pred_c[1][c][..]) } else { None };
            let plane = if c == 0 { &mut self.pic.planes.cb } else { &mut self.pic.planes.cr };
            inter::weighted_store(&mut plane[cy * cs + cx..], cs, a, b, 8, bw / 2, bh >> fmt.chroma_y_shift, wc, fmt.max_c());
        }
    }

    /// All 16 blocks of the current MB share references and motion vectors.
    fn uniform_motion(&self) -> bool {
        let st = self.mb();
        (0..2).all(|l| {
            let r = st.ref_idx[l];
            r[1] == r[0] && r[2] == r[0] && r[3] == r[0] && st.mv[l].iter().all(|m| *m == st.mv[l][0])
        })
    }

    /// Inter prediction for the whole current MB, using the stored motion.
    pub fn predict_inter(&mut self) {
        let info = self.cur.info;
        let direct_unit = if self.sps.direct_8x8_inference { 2 } else { 1 };
        match info.kind {
            MbKind::PSkip => self.mc_rect(0, 0, 4, 4),
            MbKind::BSkip | MbKind::BDirect16x16 if self.uniform_motion() => self.mc_rect(0, 0, 4, 4),
            MbKind::BSkip | MbKind::BDirect16x16 => {
                for y in (0..4).step_by(direct_unit) {
                    for x in (0..4).step_by(direct_unit) {
                        self.mc_rect(x, y, direct_unit, direct_unit);
                    }
                }
            }
            _ => {
                if info.part != Part::P8x8 {
                    for p in 0..info.part.num_parts() {
                        let (x, y, w, h) = info.part.rect(p);
                        self.mc_rect(x, y, w, h);
                    }
                } else {
                    for p in 0..4 {
                        let sub = self.cur.sub[p];
                        let (x8, y8) = ((p & 1) * 2, (p >> 1) * 2);
                        if sub.direct {
                            for y in (0..2).step_by(direct_unit) {
                                for x in (0..2).step_by(direct_unit) {
                                    self.mc_rect(x8 + x, y8 + y, direct_unit, direct_unit);
                                }
                            }
                        } else {
                            for j in 0..sub.shape.num_parts() {
                                let (sx, sy, w, h) = sub.shape.rect(j);
                                self.mc_rect(x8 + sx, y8 + sy, w, h);
                            }
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // Intra prediction mode derivation (8.3.1.1 / 8.3.2.1)
    // ------------------------------------------------------------------------------------------

    fn intra_mode_nb(&self, n: usize, bx: usize, by: usize) -> Option<u8> {
        // returns Some(mode) or None if dcPredModePredictedFlag must be set
        let m = self.nb[n]?;
        let st = &self.pic.mbs[m];
        if self.pps.constrained_intra_pred && !st.kind.is_intra() {
            return None;
        }
        Some(match st.kind {
            MbKind::I4x4 | MbKind::I8x8 => st.intra_modes[by * 4 + bx],
            _ => 2,
        })
    }

    /// Resolve Intra4x4PredMode / Intra8x8PredMode for the current MB from `self.cur.rem_mode`.
    pub fn derive_intra_modes(&mut self, is8x8: bool) {
        let step = if is8x8 { 2 } else { 1 };
        let nblk = if is8x8 { 4 } else { 16 };
        for i in 0..nblk {
            let (bx, by) = if is8x8 {
                ((i & 1) * 2, (i >> 1) * 2)
            } else {
                let (x, y) = BLK4_XY[i];
                (x as usize, y as usize)
            };
            let a = if bx > 0 { Some(self.mb().intra_modes[by * 4 + bx - 1]) } else { self.intra_mode_nb(0, 3, by) };
            let b = if by > 0 { Some(self.mb().intra_modes[(by - 1) * 4 + bx]) } else { self.intra_mode_nb(1, bx, 3) };
            let pred = match (a, b) {
                (Some(a), Some(b)) => a.min(b),
                _ => 2,
            };
            let rem = self.cur.rem_mode[i];
            let mode = if rem < 0 {
                pred
            } else if (rem as u8) < pred {
                rem as u8
            } else {
                rem as u8 + 1
            };
            let st = self.mb_mut();
            for yy in by..by + step {
                for xx in bx..bx + step {
                    st.intra_modes[yy * 4 + xx] = mode;
                }
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // Reconstruction
    // ------------------------------------------------------------------------------------------

    fn add_luma_block4(&mut self, raster: usize) {
        if self.s.blk_nz & (1 << raster) == 0 {
            return;
        }
        let stride = self.pic.planes.width;
        let px = self.mb_x * 16 + (raster & 3) * 4;
        let py = self.mb_y * 16 + (raster >> 2) * 4;
        let Some(blk) = self.s.coef.get_mut(raster * 16..).and_then(|c| c.first_chunk_mut::<16>()) else {
            return;
        };
        let max = self.pic.planes.fmt.max_y();
        transform::idct4_add(blk, &mut self.pic.planes.y[py * stride + px..], stride, max);
        blk.fill(0);
    }

    fn add_luma_block8(&mut self, b8: usize) {
        if self.s.blk_nz & (1 << b8) == 0 {
            return;
        }
        let stride = self.pic.planes.width;
        let px = self.mb_x * 16 + (b8 & 1) * 8;
        let py = self.mb_y * 16 + (b8 >> 1) * 8;
        let Some(blk) = self.s.coef.get_mut(b8 * 64..).and_then(|c| c.first_chunk_mut::<64>()) else {
            return;
        };
        let max = self.pic.planes.fmt.max_y();
        transform::idct8_add(blk, &mut self.pic.planes.y[py * stride + px..], stride, max);
        blk.fill(0);
    }

    fn add_chroma(&mut self) {
        let stride = self.pic.planes.cwidth;
        let fmt = self.pic.planes.fmt;
        // 4x4 chroma blocks per component: 2x2 in 4:2:0, 2x4 in 4:2:2
        let nblk = 4usize << usize::from(fmt.chroma_y_shift == 0);
        for c in 0..2 {
            if self.s.c_nz[c] == 0 {
                continue;
            }
            for b in 0..nblk {
                if self.s.c_nz[c] & (1 << b) == 0 {
                    continue;
                }
                let px = self.mb_x * 8 + (b & 1) * 4;
                let py = self.mb_y * (16 >> fmt.chroma_y_shift) + (b >> 1) * 4;
                let Some(blk) = self.s.coef_c[c].get_mut(b * 16..).and_then(|c| c.first_chunk_mut::<16>()) else {
                    continue;
                };
                let plane = if c == 0 { &mut self.pic.planes.cb } else { &mut self.pic.planes.cr };
                transform::idct4_add(blk, &mut plane[py * stride + px..], stride, fmt.max_c());
                blk.fill(0);
            }
            self.s.c_nz[c] = 0;
        }
    }

    fn luma_avail4(&self, bx: usize, by: usize) -> Avail {
        let a = self.intra_nb(0);
        let b = self.intra_nb(1);
        let c = self.intra_nb(2);
        let d = self.intra_nb(3);
        let left = if bx > 0 { true } else { a };
        let top = if by > 0 { true } else { b };
        let top_left = match (bx > 0, by > 0) {
            (true, true) => true,
            (false, true) => a,
            (true, false) => b,
            (false, false) => d,
        };
        let top_right = if by == 0 {
            if bx < 3 { b } else { c }
        } else if bx == 3 {
            false
        } else {
            RASTER_TO_BLK4[(by - 1) * 4 + bx + 1] < RASTER_TO_BLK4[by * 4 + bx]
        };
        Avail { left, top, top_left, top_right }
    }

    /// Reconstruct an intra MB (prediction + residual) after its syntax has been parsed.
    pub fn reconstruct_intra(&mut self) {
        let kind = self.mb().kind;
        let fmt = self.pic.planes.fmt;
        let stride = self.pic.planes.width;
        let (x0, y0) = (self.mb_x * 16, self.mb_y * 16);
        match kind {
            MbKind::I4x4 => {
                for (blk, &(bx, by)) in BLK4_XY.iter().enumerate() {
                    let (bx, by) = (bx as usize, by as usize);
                    let _ = blk;
                    let av = self.luma_avail4(bx, by);
                    let mode = self.mb().intra_modes[by * 4 + bx];
                    intra::pred4x4(&mut self.pic.planes.y, stride, x0 + bx * 4, y0 + by * 4, mode, av, fmt.max_y());
                    self.add_luma_block4(by * 4 + bx);
                }
            }
            MbKind::I8x8 => {
                for b8 in 0..4 {
                    let (bx, by) = ((b8 & 1) * 2, (b8 >> 1) * 2);
                    let mut av = self.luma_avail4(bx, by);
                    av.top_right = match b8 {
                        0 => self.intra_nb(1),
                        1 => self.intra_nb(2),
                        2 => true,
                        _ => false,
                    };
                    let mode = self.mb().intra_modes[by * 4 + bx];
                    intra::pred8x8(&mut self.pic.planes.y, stride, x0 + bx * 4, y0 + by * 4, mode, av, fmt.max_y());
                    self.add_luma_block8(b8);
                }
            }
            MbKind::I16x16 => {
                let av = Avail { left: self.intra_nb(0), top: self.intra_nb(1), top_left: self.intra_nb(3), top_right: false };
                intra::pred16x16(&mut self.pic.planes.y, stride, x0, y0, self.cur.info.i16_mode, av, fmt.max_y());
                for r in 0..16 {
                    self.add_luma_block4(r);
                }
            }
            _ => {}
        }
        let av = Avail { left: self.intra_nb(0), top: self.intra_nb(1), top_left: self.intra_nb(3), top_right: false };
        let cs = self.pic.planes.cwidth;
        let mode = self.mb().intra_chroma_mode;
        let (cx, cy) = (self.mb_x * 8, self.mb_y * (16 >> fmt.chroma_y_shift));
        intra::pred_chroma(&mut self.pic.planes.cb, cs, cx, cy, mode, av, fmt.max_c(), fmt.chroma_y_shift == 0);
        intra::pred_chroma(&mut self.pic.planes.cr, cs, cx, cy, mode, av, fmt.max_c(), fmt.chroma_y_shift == 0);
        self.add_chroma();
        self.s.blk_nz = 0;
    }

    /// Add the residual of an inter MB (prediction already written).
    pub fn reconstruct_inter_residual(&mut self) {
        if self.mb().transform_8x8 {
            for b8 in 0..4 {
                self.add_luma_block8(b8);
            }
        } else if self.s.blk_nz != 0 {
            for r in 0..16 {
                self.add_luma_block4(r);
            }
        }
        self.add_chroma();
        self.s.blk_nz = 0;
    }

    /// Write I_PCM samples (8.3.5, direct assignment): `samples` = [256 luma][MbWidthC*MbHeightC
    /// Cb][same Cr] with the stream's bit depths already applied.
    pub fn write_pcm(&mut self, samples: &[u16]) {
        let fmt = self.pic.planes.fmt;
        let stride = self.pic.planes.width;
        let (x0, y0) = (self.mb_x * 16, self.mb_y * 16);
        let clines = fmt.chroma_row_lines();
        let n = 8 * clines;
        if samples.len() < 256 + 2 * n {
            return;
        }
        for r in 0..16 {
            self.pic.planes.y[(y0 + r) * stride + x0..(y0 + r) * stride + x0 + 16].copy_from_slice(&samples[r * 16..r * 16 + 16]);
        }
        let cs = self.pic.planes.cwidth;
        let (cx, cy) = (self.mb_x * 8, self.mb_y * clines);
        for c in 0..2 {
            let src = &samples[256 + c * n..256 + c * n + n];
            let plane = if c == 0 { &mut self.pic.planes.cb } else { &mut self.pic.planes.cr };
            for r in 0..clines {
                plane[(cy + r) * cs + cx..(cy + r) * cs + cx + 8].copy_from_slice(&src[r * 8..r * 8 + 8]);
            }
        }
    }

    /// Common state for an I_PCM MB (its QPs are 0 / QPC of QPY 0, 8.7.2.3).
    pub fn finish_pcm(&mut self) {
        let qpc = [self.chroma_qp(0, 0) as u8, self.chroma_qp(0, 1) as u8];
        let qp0 = self.sps.qp_bd_offset_y() as u8;
        let st = self.mb_mut();
        st.kind = MbKind::IPcm;
        st.qp = qp0;
        st.qpc = qpc;
        st.nnz = [16; 16];
        st.nnz_c = [[16; 8]; 2];
        st.cbf_dc = 7;
        st.cbp = 0x2f;
        st.nz_mask = 0xffff;
        st.ref_idx = [[-1; 4]; 2];
    }

    /// Update QP after `mb_qp_delta` (7.4.4 / 8.5.6, equation 7-10: the QP wraps over
    /// `52 + QpBdOffsetY` values).
    ///
    /// The bitstream constraint (7-4) is on the *result*, not on the delta: `QPY` only has to stay
    /// inside `-QpBdOffsetY..=51`, so with a deep `QpBdOffsetY` a single `mb_qp_delta` may legally
    /// exceed +25 — a 10-bit stream can move a whole `51 + 12` at once (7-4 with 7-10). Rejecting
    /// `|delta| > 26` outright therefore rejects valid deep-bit-depth streams; only a delta that
    /// cannot land inside the wrap range from any previous QP is corrupt.
    pub fn apply_qp_delta(&mut self, delta: i32) -> Result<()> {
        let qpb = self.sps.qp_bd_offset_y();
        let span = 52 + qpb;
        ensure!((-span..=span).contains(&delta), "mb_qp_delta {delta} out of range");
        let before = self.qp;
        self.qp = wrap_qp(self.qp, delta, qpb);
        if trace_on() {
            eprintln!("QP {before} {delta:+} -> {}", self.qp);
        }
        debug_assert!((-qpb..=51).contains(&self.qp), "equation 7-10 keeps QPY in range");
        Ok(())
    }

    /// Store the macroblock's QPs: the *effective* QPs (`QPY + QpBdOffsetY` / `QpC + QpBdOffsetC`)
    /// in 0..=63, which is what residual scaling and deblocking derive from.
    ///
    /// `self.qp` is the `QPY` of 7-4 (`-QpBdOffsetY..=51`); [`Self::chroma_qp`] takes the effective
    /// luma QP and applies `QpBdOffsetC` itself (8-309..8-312).
    pub fn store_qp(&mut self) {
        let qp = self.qp;
        let qpc = [self.chroma_qp(qp, 0) as u8, self.chroma_qp(qp, 1) as u8];
        let qp_eff = qp + self.sps.qp_bd_offset_y();
        let st = self.mb_mut();
        st.qp = qp_eff as u8;
        st.qpc = qpc;
    }

    // ------------------------------------------------------------------------------------------
    // Residual storage helpers (shared by CAVLC and CABAC)
    // ------------------------------------------------------------------------------------------

    fn luma_list4(&self) -> usize {
        if self.mb().kind.is_intra() { 0 } else { 3 }
    }

    /// Store a 4x4 luma block given in scan order (levels[0..16]); `ac_only` skips position 0.
    pub fn put_luma4(&mut self, raster: usize, levels: &[i32; 16], ac_only: bool) {
        let qp = self.qp + self.sps.qp_bd_offset_y();
        let ls = &self.ls.ls4[self.luma_list4()][(qp % 6) as usize];
        let blk = &mut self.s.coef[raster * 16..raster * 16 + 16];
        let mut any = false;
        for (k, &lv) in levels.iter().enumerate().skip(ac_only as usize) {
            if lv != 0 {
                let p = ZIGZAG4[k] as usize;
                blk[p] = transform::scale4(lv, ls[p], qp);
                any = true;
            }
        }
        if any {
            self.s.blk_nz |= 1 << raster;
            self.pic.mbs[self.mb_addr].nz_mask |= 1 << raster;
        }
    }

    /// Store an 8x8 luma block given in scan order.
    pub fn put_luma8(&mut self, b8: usize, levels: &[i32; 64]) {
        let qp = self.qp + self.sps.qp_bd_offset_y();
        let list = if self.mb().kind.is_intra() { 0 } else { 1 };
        let ls = &self.ls.ls8[list][(qp % 6) as usize];
        let blk = &mut self.s.coef[b8 * 64..b8 * 64 + 64];
        let mut any = false;
        for (k, &lv) in levels.iter().enumerate() {
            if lv != 0 {
                let p = ZIGZAG8[k] as usize;
                blk[p] = transform::scale8(lv, ls[p], qp);
                any = true;
            }
        }
        if any {
            self.s.blk_nz |= 1 << b8;
            let (bx, by) = ((b8 & 1) * 2, (b8 >> 1) * 2);
            let m = (0b11u16 << (by * 4 + bx)) | (0b11u16 << ((by + 1) * 4 + bx));
            self.pic.mbs[self.mb_addr].nz_mask |= m;
        }
    }

    /// Intra16x16 DC levels in scan order -> scaled DC of each 4x4 block.
    pub fn put_luma_dc(&mut self, levels: &[i32; 16]) {
        let mut c = [0i32; 16];
        for (k, &lv) in levels.iter().enumerate() {
            c[ZIGZAG4[k] as usize] = lv;
        }
        let qp = self.qp + self.sps.qp_bd_offset_y();
        let ls00 = self.ls.ls4[0][(qp % 6) as usize][0];
        transform::luma_dc_dequant(&mut c, qp, ls00);
        for (r, &dc) in c.iter().enumerate() {
            if dc != 0 {
                self.s.coef[r * 16] = dc;
                self.s.blk_nz |= 1 << r;
            }
        }
    }

    /// Chroma DC levels for component `c`: 4 values in scan order for 4:2:0, 8 for 4:2:2.
    pub fn put_chroma_dc(&mut self, c: usize, levels: &[i32]) {
        let qpc = self.chroma_qp(self.qp, c);
        let list = if self.mb().kind.is_intra() { 1 + c } else { 4 + c };
        let four_two_two = self.pic.planes.fmt.chroma_y_shift == 0;
        let count = if four_two_two { 8 } else { 4 };
        let at = |k: usize| levels.get(k).copied().unwrap_or(0);
        let mut v = [0i32; 8];
        if four_two_two {
            // 8-305: the 2x4 array c in raster order from the scan-ordered levels:
            // [[L0, L2], [L1, L5], [L3, L6], [L4, L7]] (rows i = 0..4, columns j = 0..2).
            v = [at(0), at(2), at(1), at(5), at(3), at(6), at(4), at(7)];
            // scaling uses qPDC = qP + 3 (8-327)
            let ls00 = self.ls.ls4[list][((qpc + 3) % 6) as usize][0];
            transform::chroma_dc_dequant_422(&mut v, qpc, ls00);
        } else {
            let mut v4 = [at(0), at(1), at(2), at(3)];
            let ls00 = self.ls.ls4[list][(qpc % 6) as usize][0];
            transform::chroma_dc_dequant_420(&mut v4, qpc, ls00);
            v[..4].copy_from_slice(&v4);
        }
        // Figure 8-7 with 6.4.7: dcC[i][j] goes to block i * 2 + j (x = j * 4, y = i * 4).
        for (b, &dc) in v[..count].iter().enumerate() {
            if dc != 0 {
                self.s.coef_c[c][b * 16] = dc;
                self.s.c_nz[c] |= 1 << b;
            }
        }
    }

    /// Chroma AC levels (scan positions 1..16 in levels[1..]) for block `b` of component `c`.
    pub fn put_chroma_ac(&mut self, c: usize, b: usize, levels: &[i32; 16]) {
        let qpc = self.chroma_qp(self.qp, c);
        let list = if self.mb().kind.is_intra() { 1 + c } else { 4 + c };
        let ls = &self.ls.ls4[list][(qpc % 6) as usize];
        let blk = &mut self.s.coef_c[c][b * 16..b * 16 + 16];
        let mut any = false;
        for (k, &lv) in levels.iter().enumerate().skip(1) {
            if lv != 0 {
                let p = ZIGZAG4[k] as usize;
                blk[p] = transform::scale4(lv, ls[p], qpc);
                any = true;
            }
        }
        if any {
            self.s.c_nz[c] |= 1 << b;
        }
    }

    // ------------------------------------------------------------------------------------------
    // CAVLC macroblock layer
    // ------------------------------------------------------------------------------------------

    /// Decode a P_Skip or B_Skip macroblock at the current address.
    pub fn decode_skip(&mut self) -> Result<()> {
        let b = self.sh.slice_type.is_b();
        self.cur = MbCur::default();
        self.cur.info.kind = if b { MbKind::BSkip } else { MbKind::PSkip };
        self.mb_mut().kind = self.cur.info.kind;
        self.store_qp();
        if b {
            self.derive_direct(0xf)?;
        } else {
            ensure!(!self.refs[0].is_empty(), "P_Skip with empty reference list");
            self.derive_p_skip();
        }
        self.predict_inter();
        self.prev_qp_delta_nz = false;
        Ok(())
    }

    fn read_te(r: &mut BitReader, range: u32) -> Result<u32> {
        Ok(if range > 1 { r.read_ue()? } else { !r.read_bit()? as u32 })
    }

    /// Parse and reconstruct one macroblock_layer() with CAVLC.
    pub fn decode_mb_cavlc(&mut self, r: &mut BitReader) -> Result<()> {
        let mb_type = r.read_ue()?;
        let st = self.sh.slice_type;
        let info = match st {
            SliceType::I | SliceType::Si => intra_mb_type(mb_type),
            SliceType::P | SliceType::Sp => {
                if mb_type < 5 {
                    p_mb_type(mb_type)
                } else {
                    intra_mb_type(mb_type - 5)
                }
            }
            SliceType::B => {
                if mb_type < 23 {
                    b_mb_type(mb_type)
                } else {
                    intra_mb_type(mb_type - 23)
                }
            }
        };
        let Some(mut info) = info else { return invalid(format!("mb_type {mb_type} out of range")) };
        self.cur = MbCur { info, ..Default::default() };
        if info.kind == MbKind::IPcm {
            r.byte_align();
            let (samples, end) = read_pcm_samples(r.data(), r.byte_pos() * 8, self.pic.planes.fmt)?;
            r.seek_bits(end);
            self.finish_pcm();
            self.write_pcm(&samples);
            return Ok(());
        }
        let mut transform_8x8 = false;
        if info.kind == MbKind::I4x4 && self.pps.transform_8x8_mode && r.read_flag()? {
            transform_8x8 = true;
            info.kind = MbKind::I8x8;
            self.cur.info.kind = MbKind::I8x8;
        }
        self.mb_mut().kind = info.kind;
        self.mb_mut().transform_8x8 = transform_8x8;
        let n_ref = [self.sh.num_ref_idx_active[0], self.sh.num_ref_idx_active[1]];
        match info.kind {
            MbKind::I4x4 | MbKind::I8x8 => {
                let n = if info.kind == MbKind::I8x8 { 4 } else { 16 };
                for i in 0..n {
                    self.cur.rem_mode[i] = if r.read_flag()? { -1 } else { r.read_bits(3)? as i8 };
                }
                self.derive_intra_modes(info.kind == MbKind::I8x8);
                let m = r.read_ue()?;
                ensure!(m <= 3, "intra_chroma_pred_mode out of range");
                self.mb_mut().intra_chroma_mode = m as u8;
            }
            MbKind::I16x16 => {
                let m = r.read_ue()?;
                ensure!(m <= 3, "intra_chroma_pred_mode out of range");
                self.mb_mut().intra_chroma_mode = m as u8;
            }
            MbKind::BDirect16x16 => {}
            _ => {
                if info.part == Part::P8x8 {
                    for p in 0..4 {
                        let t = r.read_ue()?;
                        let sub = if st.is_b() { b_sub_mb_type(t) } else { p_sub_mb_type(t) };
                        let Some(sub) = sub else { return invalid(format!("sub_mb_type {t} out of range")) };
                        self.cur.sub[p] = sub;
                    }
                    for l in 0..2 {
                        for p in 0..4 {
                            let sub = self.cur.sub[p];
                            if sub.direct || sub.pred & (1 << l) == 0 {
                                continue;
                            }
                            self.cur.ref_idx[l][p] = if n_ref[l] > 1 && !info.ref0 { Self::read_te(r, n_ref[l] - 1)? as i8 } else { 0 };
                        }
                    }
                    for l in 0..2 {
                        for p in 0..4 {
                            let sub = self.cur.sub[p];
                            if sub.direct || sub.pred & (1 << l) == 0 {
                                continue;
                            }
                            for j in 0..sub.shape.num_parts() {
                                let x = r.read_se()?;
                                let y = r.read_se()?;
                                self.cur.mvd[l][p * 4 + j] = [x as i16, y as i16];
                            }
                        }
                    }
                } else {
                    let np = info.part.num_parts();
                    for l in 0..2 {
                        for p in 0..np {
                            if info.pred[p] & (1 << l) != 0 {
                                self.cur.ref_idx[l][p] = if n_ref[l] > 1 { Self::read_te(r, n_ref[l] - 1)? as i8 } else { 0 };
                            }
                        }
                    }
                    for l in 0..2 {
                        for p in 0..np {
                            if info.pred[p] & (1 << l) != 0 {
                                let x = r.read_se()?;
                                let y = r.read_se()?;
                                self.cur.mvd[l][p * 4] = [x as i16, y as i16];
                            }
                        }
                    }
                }
            }
        }
        // coded_block_pattern
        let cbp = if info.kind == MbKind::I16x16 {
            info.i16_cbp
        } else {
            let code = r.read_ue()?;
            ensure!(code < 48, "coded_block_pattern out of range");
            let (i, p) = CBP_ME[code as usize];
            if info.kind.is_intra() { i } else { p }
        };
        self.cur.cbp = cbp;
        self.mb_mut().cbp = cbp;
        if !info.kind.is_intra() && cbp & 15 != 0 && self.pps.transform_8x8_mode && self.no_sub_8x8_lt() {
            let t = r.read_flag()?;
            self.mb_mut().transform_8x8 = t;
        }
        if !info.kind.is_intra() {
            self.derive_inter_motion()?;
        }
        if cbp != 0 || info.kind == MbKind::I16x16 {
            let d = r.read_se()?;
            self.apply_qp_delta(d)?;
        }
        self.store_qp();
        self.residual_cavlc(r, cbp)?;
        if info.kind.is_intra() {
            self.reconstruct_intra();
        } else {
            self.predict_inter();
            self.reconstruct_inter_residual();
        }
        Ok(())
    }

    /// Whether transform_size_8x8_flag may be present for an inter MB (NoSubMbPartSizeLessThan8x8 etc.).
    pub fn no_sub_8x8_lt(&self) -> bool {
        let info = self.cur.info;
        match info.kind {
            MbKind::BDirect16x16 => self.sps.direct_8x8_inference,
            MbKind::Inter if info.part == Part::P8x8 => {
                self.cur.sub.iter().all(|s| if s.direct { self.sps.direct_8x8_inference } else { s.shape == SubPart::S8x8 })
            }
            _ => true,
        }
    }

    fn residual_cavlc(&mut self, r: &mut BitReader, cbp: u8) -> Result<()> {
        let kind = self.mb().kind;
        let t8 = self.mb().transform_8x8;
        if kind == MbKind::I16x16 {
            let nc = self.nc_luma(0);
            let mut lv = [0i32; 16];
            cavlc::residual_block(r, &mut lv, 0, 15, 16, nc)?;
            self.put_luma_dc(&lv);
        }
        for b8 in 0..4 {
            let (bx8, by8) = ((b8 & 1) * 2, (b8 >> 1) * 2);
            if cbp & (1 << b8) == 0 {
                continue;
            }
            if t8 {
                let mut lv8 = [0i32; 64];
                for i4 in 0..4 {
                    let raster = (by8 + (i4 >> 1)) * 4 + bx8 + (i4 & 1);
                    let nc = self.nc_luma(raster);
                    let mut lv = [0i32; 16];
                    let tc = cavlc::residual_block(r, &mut lv, 0, 15, 16, nc)?;
                    self.mb_mut().nnz[raster] = tc;
                    for (i, &v) in lv.iter().enumerate() {
                        lv8[4 * i + i4] = v;
                    }
                }
                self.put_luma8(b8, &lv8);
            } else {
                for i4 in 0..4 {
                    let raster = (by8 + (i4 >> 1)) * 4 + bx8 + (i4 & 1);
                    let nc = self.nc_luma(raster);
                    let mut lv = [0i32; 16];
                    let tc = if kind == MbKind::I16x16 {
                        cavlc::residual_block(r, &mut lv[1..], 0, 14, 15, nc)?
                    } else {
                        cavlc::residual_block(r, &mut lv, 0, 15, 16, nc)?
                    };
                    self.mb_mut().nnz[raster] = tc;
                    self.put_luma4(raster, &lv, kind == MbKind::I16x16);
                }
            }
        }
        let cbp_c = cbp >> 4;
        // 4x4 chroma blocks per component: 2x2 in 4:2:0, 2x4 in 4:2:2
        let nblk = 4usize << usize::from(self.pic.planes.fmt.chroma_y_shift == 0);
        if cbp_c & 3 != 0 {
            for c in 0..2 {
                let mut lv = [0i32; 8];
                if nblk == 8 {
                    // 4:2:2 chroma DC: 8 coefficients (4 * NumC8x8, NumC8x8 = 2), nC = -2
                    cavlc::residual_block(r, &mut lv, 0, 7, 8, -2)?;
                } else {
                    cavlc::residual_block(r, &mut lv, 0, 3, 4, -1)?;
                }
                self.put_chroma_dc(c, &lv[..nblk]);
            }
        }
        if cbp_c & 2 != 0 {
            for c in 0..2 {
                for b in 0..nblk {
                    let nc = self.nc_chroma(c, b);
                    let mut lv = [0i32; 16];
                    let tc = cavlc::residual_block(r, &mut lv[1..], 0, 14, 15, nc)?;
                    self.mb_mut().nnz_c[c][b] = tc;
                    self.put_chroma_ac(c, b, &lv);
                }
            }
        }
        Ok(())
    }

    /// Decode slice_data() with CAVLC. `r` is positioned after the slice header.
    pub fn decode_cavlc(&mut self, r: &mut BitReader) -> Result<()> {
        let total = self.mb_w * self.mb_h;
        let mut addr = self.sh.first_mb_in_slice as usize;
        let intra_slice = self.sh.slice_type.is_intra();
        loop {
            ensure!(addr < total, "macroblock address {addr} out of range");
            if !intra_slice {
                let run = r.read_ue()? as usize;
                ensure!(addr + run <= total, "mb_skip_run out of range");
                for _ in 0..run {
                    self.start_mb(addr);
                    self.decode_skip()?;
                    self.pic.mb_done(addr);
                    addr += 1;
                }
                if run > 0 && !r.more_rbsp_data() {
                    break;
                }
                ensure!(addr < total, "macroblock address {addr} out of range");
            }
            self.start_mb(addr);
            self.decode_mb_cavlc(r)?;
            self.pic.mb_done(addr);
            addr += 1;
            if !r.more_rbsp_data() {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod qp_delta_tests {
    use super::wrap_qp;

    /// 8-bit (`QpBdOffsetY = 0`): `QPY` is the effective QP over 0..=51 and wraps over 52 values.
    #[test]
    fn eight_bit_wraps_over_52() {
        assert_eq!(wrap_qp(26, 0, 0), 26);
        assert_eq!(wrap_qp(26, 25, 0), 51);
        assert_eq!(wrap_qp(26, -26, 0), 0);
        assert_eq!(wrap_qp(51, 1, 0), 0); // wraps past the top
        assert_eq!(wrap_qp(0, -1, 0), 51); // wraps past the bottom
    }

    /// 10-bit (`QpBdOffsetY = 12`): `QPY` runs over -12..=51, so one delta may legally move further
    /// than the 8-bit -26..=25 bound — the case a `|delta| <= 26` range check used to reject.
    #[test]
    fn ten_bit_allows_deltas_beyond_the_eight_bit_bound() {
        assert_eq!(wrap_qp(-12, 37, 12), 25); // -12 -> +25 is a legal +37 step
        assert_eq!(wrap_qp(26, 25, 12), 51);
        assert_eq!(wrap_qp(51, 1, 12), -12); // wraps past the top of the 10-bit range
        assert_eq!(wrap_qp(-12, -1, 12), 51); // and past the bottom
    }

    /// Whatever the bit depth and step, the result stays inside the 7-4 range, so callers can rely
    /// on it for scaling-matrix indices and the deblocking tables.
    #[test]
    fn result_is_always_in_the_seven_four_range() {
        for qpb in [0, 6, 12] {
            let span = 52 + qpb;
            for qp in -qpb..=51 {
                for delta in -span..=span {
                    let out = wrap_qp(qp, delta, qpb);
                    assert!((-qpb..=51).contains(&out), "qpb {qpb} qp {qp} delta {delta} -> {out}");
                }
            }
        }
    }
}
