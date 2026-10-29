//! Frame decoding: macroblock prediction records (11, 16, 17), coefficients (13), reconstruction
//! (12, 14, 18), the loop filter (15) and reference frame management (9.7, 9.8).

use std::sync::Arc;

use crate::Picture;
use crate::boolcoder::BoolDecoder;
use crate::error::{Error, Result, ensure};
use crate::header::{EntropyContext, FrameHeader, FrameTag, LfDeltas, Segmentation, read_header};
use crate::loopfilter::{self, Kind, Params};
use crate::predict::{self, BlockRect, DstPlane, RefPlane};
use crate::spec_tables::{
    AC_QLOOKUP, BILINEAR_FILTERS, BMODE_PROBS, COEFF_BANDS, DC_QLOOKUP, KF_BMODE_PROBS, KF_UV_MODE_PROBS, KF_YMODE_PROBS, MODE_CONTEXTS, MVPARTITION_PROBS,
    SIXTAP_FILTERS, SUB_MV_REF_PROBS,
};
use crate::tables::{
    ABOVE_4X4, B_DC_PRED, B_HE_PRED, B_PRED, B_TM_PRED, B_VE_PRED, BMODE_TREE, CAT_BASE, COEFF_TREE, DC_PRED, DCT_0, DCT_4, DCT_CAT1, DCT_EOB, H_PRED,
    KF_YMODE_TREE, LEFT_4X4, MV_NEAR, MV_NEAREST, MV_NEW, MV_REF_TREE, MV_SPLIT, MV_ZERO, MVP_BITS, MVP_IS_SHORT, MVP_SHORT, MVP_SIGN, MVPARTITION_TREE, PCAT,
    SEGMENT_TREE, SMALL_MV_TREE, SPLIT_COUNTS, SPLIT_MAPS, SUB_MV_REF_TREE, TM_PRED, UV_MODE_TREE, V_PRED, YMODE_TREE, ZERO_4X4, ZIGZAG,
};
use crate::transform;

/// Largest frame dimension in macroblocks (14-bit sizes).
const MAX_MBS: usize = 16384 / 16 + 1;
/// Maximum padded luma samples retained by one VP8 reference picture (about 8K UHD).
/// 2^26 luma samples: 8192x8192, above every VP8 frame a camera or encoder writes (8192x4320 is 35M).
const MAX_LUMA_SAMPLES: usize = 1 << 26;

/// A reconstructed frame: planes sized to whole macroblocks.
#[derive(Clone)]
struct Frame {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    /// Luma width / height (multiples of 16); chroma is half.
    w: usize,
    h: usize,
}

impl Frame {
    fn new(w: usize, h: usize) -> Frame {
        Frame { y: vec![0; w * h], u: vec![0; w * h / 4], v: vec![0; w * h / 4], w, h }
    }
}

/// A motion vector in quarter luma pixels.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Mv {
    row: i32,
    col: i32,
}

impl Mv {
    const ZERO: Mv = Mv { row: 0, col: 0 };
    fn is_zero(self) -> bool {
        self == Mv::ZERO
    }
}

/// What later macroblocks (and the loop filter) need to know about a decoded macroblock.
#[derive(Clone, Copy, Default)]
struct MbInfo {
    /// Intra mode (`DC_PRED`..`B_PRED`) or inter mode (`MV_NEAREST`..`MV_SPLIT`).
    mode: u8,
    /// 0 intra, 1 last, 2 golden, 3 altref.
    ref_frame: u8,
    mv: Mv,
    /// Motion vector of each Y subblock (all `mv` unless split).
    bmv: [Mv; 16],
}

/// Statistics of the frames decoded so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeStats {
    pub frames: u64,
    pub key_frames: u64,
    pub hidden_frames: u64,
}

/// Dequantization factors of one segment: [DC, AC] for Y, Y2 and chroma (14.1).
#[derive(Clone, Copy, Default)]
struct Dequant {
    y: [i32; 2],
    y2: [i32; 2],
    uv: [i32; 2],
}

impl Dequant {
    fn new(q: i32, h: &FrameHeader) -> Dequant {
        let dc = |d: i32| i32::from(DC_QLOOKUP[(q + d).clamp(0, 127) as usize]);
        let ac = |d: i32| i32::from(AC_QLOOKUP[(q + d).clamp(0, 127) as usize]);
        let qi = &h.quant;
        Dequant {
            y: [dc(qi.y_dc_delta), ac(0)],
            y2: [dc(qi.y2_dc_delta) * 2, (ac(qi.y2_ac_delta) * 155 / 100).max(8)],
            uv: [dc(qi.uv_dc_delta).min(132), ac(qi.uv_ac_delta)],
        }
    }
}

/// VP8 decoder (RFC 6386). Feed it one compressed frame at a time ([`Decoder::decode`]).
pub struct Decoder {
    ctx: EntropyContext,
    seg: Segmentation,
    lf: LfDeltas,
    /// Segment of each macroblock (persists while the map is not updated).
    seg_map: Vec<u8>,
    width: usize,
    height: usize,
    mb_w: usize,
    mb_h: usize,
    /// Last, golden and altref frames.
    refs: [Option<Arc<Frame>>; 3],
    /// `ref_frame_sign_bias`, indexed by reference frame (0 intra, 1 last, 2 golden, 3 altref).
    sign_bias: [bool; 4],
    stats: DecodeStats,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            ctx: EntropyContext::default(),
            seg: Segmentation::default(),
            lf: LfDeltas::default(),
            seg_map: Vec::new(),
            width: 0,
            height: 0,
            mb_w: 0,
            mb_h: 0,
            refs: [None, None, None],
            sign_bias: [false; 4],
            stats: DecodeStats::default(),
        }
    }

    /// Forget the reference frames (after a seek): decoding resumes at the next key frame.
    pub fn reset(&mut self) {
        self.refs = [None, None, None];
    }

    pub fn stats(&self) -> DecodeStats {
        self.stats
    }

    /// Whether leaving `frame` out, decoded next, would change no other picture: an inter frame
    /// that refreshes and copies no reference buffer, does not update the segment map, and leaves
    /// the probabilities, segment data and loop filter deltas as they are now (or keeps its
    /// probability updates to itself). Only the frame header is read.
    pub fn is_disposable(&self, frame: &[u8]) -> bool {
        let Ok(tag) = FrameTag::parse(frame) else { return false };
        if tag.key_frame || self.refs[0].is_none() {
            return false;
        }
        let Some(first) = frame.get(tag.header_len..tag.header_len.saturating_add(tag.first_part_size)) else { return false };
        let mut d = BoolDecoder::new(first);
        let (mut seg, mut lf, mut ctx, mut saved) = (self.seg, self.lf, self.ctx.clone(), None);
        let h = read_header(&mut d, false, &mut seg, &mut lf, &mut ctx, &mut saved);
        let refs_untouched = !h.refresh_golden && !h.refresh_altref && !h.refresh_last && h.copy_to_golden == 0 && h.copy_to_altref == 0;
        let probs_kept = !h.refresh_entropy || ctx == self.ctx;
        let segments_kept = !seg.update_map && seg.absolute == self.seg.absolute && seg.quant == self.seg.quant && seg.lf_level == self.seg.lf_level;
        let deltas_kept = lf.ref_deltas == self.lf.ref_deltas && lf.mode_deltas == self.lf.mode_deltas;
        refs_untouched && probs_kept && segments_kept && deltas_kept
    }

    /// Key frame setup: frame size (with its limits) and the default state of 9.3, 9.4, 9.7,
    /// 13.4, 16.1 and 17.2.
    fn begin_key_frame(&mut self, tag: &FrameTag) -> Result<()> {
        ensure!(tag.width > 0 && tag.height > 0, "key frame of size {}x{}", tag.width, tag.height);
        let (w, h) = (usize::from(tag.width), usize::from(tag.height));
        let (mb_w, mb_h) = (w.div_ceil(16), h.div_ceil(16));
        let samples = mb_w.checked_mul(16).and_then(|w| w.checked_mul(mb_h)).and_then(|h| h.checked_mul(16));
        ensure!(samples.is_some_and(|n| n <= MAX_LUMA_SAMPLES), "frame size {}x{} exceeds the decoder memory limit", tag.width, tag.height);
        if w != self.width || h != self.height {
            self.width = w;
            self.height = h;
            self.mb_w = mb_w;
            self.mb_h = mb_h;
            self.seg_map = vec![0; self.mb_w * self.mb_h];
        }
        self.ctx = EntropyContext::default();
        self.seg.absolute = false;
        self.seg.quant = [0; 4];
        self.seg.lf_level = [0; 4];
        self.lf.ref_deltas = [0; 4];
        self.lf.mode_deltas = [0; 4];
        self.sign_bias = [false; 4];
        Ok(())
    }

    /// Dequantization factors of each of the four segments (9.6, 14.1).
    fn segment_dequant(&self, hdr: &FrameHeader) -> [Dequant; 4] {
        std::array::from_fn(|s| {
            let q = if self.seg.enabled {
                let v = i32::from(self.seg.quant[s]);
                if self.seg.absolute { v } else { hdr.quant.y_ac + v }
            } else {
                hdr.quant.y_ac
            };
            Dequant::new(q.clamp(0, 127), hdr)
        })
    }

    /// Reads the segment id (when the map is updated) and returns the segment of the macroblock.
    fn read_segment(&mut self, bd: &mut BoolDecoder, seg_idx: usize) -> usize {
        if self.seg.update_map {
            let s = bd.read_tree(&SEGMENT_TREE, &self.seg.tree_probs);
            if let Some(m) = self.seg_map.get_mut(seg_idx) {
                *m = s;
            }
        }
        if self.seg.enabled { self.seg_map.get(seg_idx).copied().unwrap_or(0) as usize & 3 } else { 0 }
    }

    /// Reference updates (9.7, 9.8): the copies first (altref, then golden from the possibly
    /// updated altref), then the refreshes with the new frame.
    fn update_references(&mut self, hdr: &FrameHeader, frame: &Arc<Frame>) {
        let [last, golden, altref] = self.refs.clone();
        let mut new_golden = golden;
        let mut new_altref = altref;
        match hdr.copy_to_altref {
            1 => new_altref = last.clone(),
            2 => new_altref = new_golden.clone(),
            _ => {}
        }
        match hdr.copy_to_golden {
            1 => new_golden = last.clone(),
            2 => new_golden = new_altref.clone(),
            _ => {}
        }
        if hdr.refresh_golden {
            new_golden = Some(frame.clone());
        }
        if hdr.refresh_altref {
            new_altref = Some(frame.clone());
        }
        let new_last = if hdr.refresh_last { Some(frame.clone()) } else { last };
        self.refs = [new_last, new_golden, new_altref];
    }

    /// Prediction record of a macroblock of an inter frame (11.2, 16).
    fn read_inter_frame_modes(&self, bd: &mut BoolDecoder, hdr: &FrameHeader, infos: &[MbInfo], pos: MbPos) -> MbModes {
        let mut modes = MbModes::default();
        if bd.read_bool(hdr.prob_intra) {
            modes.info.ref_frame = if bd.read_bool(hdr.prob_last) { 2 + u8::from(bd.read_bool(hdr.prob_golden)) } else { 1 };
            self.read_inter_modes(bd, infos, pos, &mut modes.info);
        } else {
            modes.info.mode = bd.read_tree(&YMODE_TREE, &self.ctx.ymode);
            if modes.info.mode == B_PRED {
                for m in &mut modes.bmodes {
                    *m = bd.read_tree(&BMODE_TREE, &BMODE_PROBS);
                }
            }
            modes.uv_mode = bd.read_tree(&UV_MODE_TREE, &self.ctx.uv_mode);
        }
        modes
    }

    /// Decode one compressed frame. Returns the picture when the frame is meant to be shown
    /// (`show_frame`), `None` for a hidden frame (an altref).
    pub fn decode(&mut self, data: &[u8], pts: i64) -> Result<Option<Picture>> {
        let tag = FrameTag::parse(data)?;
        ensure!(tag.first_part_size > 0, "empty first partition");
        if tag.key_frame {
            self.begin_key_frame(&tag)?;
        } else if self.refs[0].is_none() {
            return Err(Error::MissingReference("inter frame before the first key frame".into()));
        }
        ensure!(self.mb_w > 0 && self.mb_w <= MAX_MBS && self.mb_h <= MAX_MBS, "no frame size");
        let first_start = tag.header_len;
        let first_end = first_start.saturating_add(tag.first_part_size);
        ensure!(first_end <= data.len(), "first partition ({} bytes) runs past the frame ({} bytes)", tag.first_part_size, data.len());
        let mut bd = BoolDecoder::new(&data[first_start..first_end]);
        let mut saved = None;
        let hdr = read_header(&mut bd, tag.key_frame, &mut self.seg, &mut self.lf, &mut self.ctx, &mut saved);
        if !tag.key_frame {
            self.sign_bias[2] = hdr.sign_bias_golden;
            self.sign_bias[3] = hdr.sign_bias_altref;
        }

        let n = hdr.partitions;
        let mut token_decoders = split_token_partitions(data, first_end, n);

        let mut frame = Frame::new(self.mb_w * 16, self.mb_h * 16);
        let dequant = self.segment_dequant(&hdr);
        let recon = InterRecon {
            refs: self.refs.clone(),
            filters: if tag.version == 0 || tag.version > 3 { &SIXTAP_FILTERS } else { &BILINEAR_FILTERS },
            full_pixel: tag.version == 3,
        };

        let stride_info = self.mb_w + 1;
        let mut infos = vec![MbInfo::default(); stride_info * (self.mb_h + 1)];
        let mut lf_info: Vec<(u8, bool)> = vec![(0, false); self.mb_w * self.mb_h];
        // contexts: B_PRED modes above / left (key frames), non-zero flags above / left
        let mut above_bmodes = vec![[B_DC_PRED; 4]; self.mb_w];
        let mut above_nz = vec![[0u8; 9]; self.mb_w];

        for mb_y in 0..self.mb_h {
            let mut left_bmodes = [B_DC_PRED; 4];
            let mut left_nz = [0u8; 9];
            let part = mb_y % n;
            for mb_x in 0..self.mb_w {
                let pos = MbPos { idx: (mb_y + 1) * stride_info + mb_x + 1, stride: stride_info, mb_x, mb_y };
                // ---- prediction record (first partition)
                let seg_idx = mb_y * self.mb_w + mb_x;
                let segment = self.read_segment(&mut bd, seg_idx);
                let coeff_skip = hdr.skip_prob.is_some_and(|p| bd.read_bool(p));
                let modes = if tag.key_frame {
                    read_key_frame_modes(&mut bd, &above_bmodes[mb_x], &left_bmodes)
                } else {
                    self.read_inter_frame_modes(&mut bd, &hdr, &infos, pos)
                };
                let info = modes.info;
                above_bmodes[mb_x] = [modes.bmodes[12], modes.bmodes[13], modes.bmodes[14], modes.bmodes[15]];
                left_bmodes = [modes.bmodes[3], modes.bmodes[7], modes.bmodes[11], modes.bmodes[15]];
                if let Some(slot) = infos.get_mut(pos.idx) {
                    *slot = info;
                }

                // ---- residual (token partition)
                let has_y2 = info.mode != B_PRED && info.mode != MV_SPLIT;
                let residual = if coeff_skip {
                    skip_residual(has_y2, &mut above_nz[mb_x], &mut left_nz);
                    MbResidual::default()
                } else {
                    read_residual(&mut token_decoders[part], &self.ctx.coeff, &dequant[segment], has_y2, &mut above_nz[mb_x], &mut left_nz)
                };

                // ---- loop filter level (15.1, 9.3, 9.4)
                let level = self.filter_level(&hdr, segment, &info);
                let inner = !has_y2 || residual.has_coeffs;
                if let Some(l) = lf_info.get_mut(seg_idx) {
                    *l = (level, inner);
                }

                // ---- reconstruction
                reconstruct_mb(&mut frame, (mb_x, mb_y), self.mb_w, &modes, &residual, &recon)?;
            }
        }

        if hdr.filter_level > 0 {
            loop_filter(&mut frame, self.mb_w, self.mb_h, &lf_info, &hdr, tag.key_frame);
        }
        if let Some(s) = saved {
            self.ctx = s;
        }

        let frame = Arc::new(frame);
        self.update_references(&hdr, &frame);

        self.stats.frames += 1;
        self.stats.key_frames += u64::from(tag.key_frame);
        if !tag.show_frame {
            self.stats.hidden_frames += 1;
            return Ok(None);
        }
        Ok(Some(self.picture(&frame, pts, tag.key_frame, hdr.color_space)))
    }

    /// The visible part of a reconstructed frame.
    fn picture(&self, f: &Frame, pts: i64, key: bool, color_space: u8) -> Picture {
        let (w, h) = (self.width, self.height);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let crop = |plane: &[u8], stride: usize, w: usize, h: usize| {
            let mut out = Vec::with_capacity(w * h);
            for r in 0..h {
                out.extend_from_slice(plane.get(r * stride..r * stride + w).unwrap_or(&[]));
            }
            out
        };
        Picture {
            width: w as u32,
            height: h as u32,
            chroma_width: cw as u32,
            chroma_height: ch as u32,
            y: crop(&f.y, f.w, w, h),
            u: crop(&f.u, f.w / 2, cw, ch),
            v: crop(&f.v, f.w / 2, cw, ch),
            pts,
            key,
            color_space,
        }
    }

    /// Loop filter level of a macroblock (15.1 with the adjustments of 9.3 and 9.4).
    fn filter_level(&self, hdr: &FrameHeader, segment: usize, info: &MbInfo) -> u8 {
        let mut level = i32::from(hdr.filter_level);
        if self.seg.enabled {
            let v = i32::from(self.seg.lf_level[segment]);
            level = if self.seg.absolute { v } else { level + v }.clamp(0, 63);
        }
        if self.lf.enabled {
            level += i32::from(self.lf.ref_deltas[usize::from(info.ref_frame)]);
            let mode_delta = match (info.ref_frame, info.mode) {
                (0, B_PRED) => Some(0),
                (0, _) => None,
                (_, MV_ZERO) => Some(1),
                (_, MV_SPLIT) => Some(3),
                _ => Some(2),
            };
            if let Some(m) = mode_delta {
                level += i32::from(self.lf.mode_deltas[m]);
            }
            level = level.clamp(0, 63);
        }
        level as u8
    }

    /// Inter prediction record (16.2-16.4, 17) of the macroblock at `idx` in `infos`.
    fn read_inter_modes(&self, bd: &mut BoolDecoder, infos: &[MbInfo], pos: MbPos, info: &mut MbInfo) {
        let MbPos { idx, stride, mb_x, mb_y } = pos;
        let get = |i: usize| infos.get(i).copied().unwrap_or_default();
        let above = get(idx - stride);
        let left = get(idx - 1);
        let above_left = get(idx - stride - 1);
        // vp8_find_near_mvs (16.3)
        let bias = |n: &MbInfo| {
            let mut m = n.mv;
            if self.sign_bias[usize::from(n.ref_frame)] != self.sign_bias[usize::from(info.ref_frame)] {
                m.row = -m.row;
                m.col = -m.col;
            }
            m
        };
        let mut near = [Mv::ZERO; 4];
        let mut cnt = [0usize; 4];
        let mut k = 0;
        if above.ref_frame != 0 {
            if !above.mv.is_zero() {
                k += 1;
                near[k] = bias(&above);
            }
            cnt[k] += 2;
        }
        if left.ref_frame != 0 {
            if !left.mv.is_zero() {
                let m = bias(&left);
                if m != near[k] {
                    k += 1;
                    near[k] = m;
                }
                cnt[k] += 2;
            } else {
                cnt[0] += 2;
            }
        }
        if above_left.ref_frame != 0 {
            if !above_left.mv.is_zero() {
                let m = bias(&above_left);
                if m != near[k] {
                    k += 1;
                    near[k] = m;
                }
                cnt[k] += 1;
            } else {
                cnt[0] += 1;
            }
        }
        // three distinct vectors: merge the above-left one into "nearest" when equal
        if cnt[3] > 0 && near[3] == near[1] {
            cnt[1] += 1;
        }
        cnt[3] = usize::from(above.mode == MV_SPLIT) * 2 + usize::from(left.mode == MV_SPLIT) * 2 + usize::from(above_left.mode == MV_SPLIT);
        if cnt[2] > cnt[1] {
            cnt.swap(1, 2);
            near.swap(1, 2);
        }
        if cnt[1] >= cnt[0] {
            near[0] = near[1];
        }
        let bounds = MvBounds::new(mb_x, mb_y, self.mb_w, self.mb_h);
        let best = bounds.clamp(near[0]);
        let probs: [u8; 4] = std::array::from_fn(|i| MODE_CONTEXTS[cnt[i].min(5) * 4 + i]);
        info.mode = MV_NEAREST + bd.read_tree(&MV_REF_TREE, &probs);
        info.mv = match info.mode {
            MV_NEAREST => bounds.clamp(near[1]),
            MV_NEAR => bounds.clamp(near[2]),
            MV_ZERO => Mv::ZERO,
            MV_NEW => {
                let d = self.read_mv(bd);
                Mv { row: best.row + d.row, col: best.col + d.col }
            }
            _ => {
                self.read_split(bd, &left, &above, best, info);
                return;
            }
        };
        info.bmv = [info.mv; 16];
    }

    /// SPLITMV partitioning and subblock vectors (16.4).
    fn read_split(&self, bd: &mut BoolDecoder, left: &MbInfo, above: &MbInfo, best: Mv, info: &mut MbInfo) {
        let part = usize::from(bd.read_tree(&MVPARTITION_TREE, &MVPARTITION_PROBS)) & 3;
        let map = &SPLIT_MAPS[part];
        let mut part_mv = [Mv::ZERO; 16];
        for j in 0..SPLIT_COUNTS[part] {
            // first subblock of partition j
            let first = map.iter().position(|&p| usize::from(p) == j).unwrap_or(0);
            let lmv = if first & 3 == 0 { left.bmv[first + 3] } else { part_mv[usize::from(map[first - 1])] };
            let amv = if first < 4 { above.bmv[first + 12] } else { part_mv[usize::from(map[first - 4])] };
            let context = if lmv == amv {
                if amv.is_zero() { 4 } else { 3 }
            } else if amv.is_zero() {
                2
            } else if lmv.is_zero() {
                1
            } else {
                0
            };
            let mv = match bd.read_tree(&SUB_MV_REF_TREE, &SUB_MV_REF_PROBS[context * 3..context * 3 + 3]) {
                LEFT_4X4 => lmv,
                ABOVE_4X4 => amv,
                ZERO_4X4 => Mv::ZERO,
                _ => {
                    let d = self.read_mv(bd);
                    Mv { row: best.row + d.row, col: best.col + d.col }
                }
            };
            part_mv[j] = mv;
        }
        for (b, &p) in map.iter().enumerate() {
            info.bmv[b] = part_mv[usize::from(p)];
        }
        info.mv = info.bmv[15];
    }

    /// `read_mv` (17.2): row, then column.
    fn read_mv(&self, bd: &mut BoolDecoder) -> Mv {
        let row = read_mv_component(bd, &self.ctx.mv[0]);
        let col = read_mv_component(bd, &self.ctx.mv[1]);
        Mv { row, col }
    }
}

/// Position of a macroblock: its index in the bordered `infos` array (row stride `stride`) and
/// its macroblock coordinates.
#[derive(Clone, Copy)]
struct MbPos {
    idx: usize,
    stride: usize,
    mb_x: usize,
    mb_y: usize,
}

/// Prediction record of one macroblock.
struct MbModes {
    info: MbInfo,
    /// Subblock intra modes (`B_DC_PRED` unless the macroblock is `B_PRED`, or on key frames).
    bmodes: [u8; 16],
    uv_mode: u8,
}

impl Default for MbModes {
    fn default() -> Self {
        MbModes { info: MbInfo::default(), bmodes: [B_DC_PRED; 16], uv_mode: DC_PRED }
    }
}

/// Dequantized coefficients of one macroblock: 16 Y, 4 U, 4 V and the Y2 block.
#[derive(Default)]
struct MbResidual {
    coeffs: [[i16; 16]; 25],
    nonzero: [bool; 25],
    has_coeffs: bool,
}

/// What inter prediction of a frame needs: the reference frames and the sub-pixel filter setup.
struct InterRecon {
    refs: [Option<Arc<Frame>>; 3],
    filters: &'static [i32; 48],
    full_pixel: bool,
}

/// Token partitions (9.5): sizes of all but the last, then the data.
fn split_token_partitions(data: &[u8], sizes_start: usize, n: usize) -> Vec<BoolDecoder<'_>> {
    let mut pos = sizes_start.saturating_add(3 * (n - 1)).min(data.len());
    let mut parts: Vec<BoolDecoder> = Vec::with_capacity(n);
    for i in 0..n {
        let size = if i + 1 < n {
            let s = sizes_start + 3 * i;
            match data.get(s..s + 3) {
                Some(b) => usize::from(b[0]) | usize::from(b[1]) << 8 | usize::from(b[2]) << 16,
                None => 0,
            }
        } else {
            data.len() - pos
        };
        let end = pos.saturating_add(size).min(data.len());
        parts.push(BoolDecoder::new(&data[pos..end]));
        pos = end;
    }
    parts
}

/// Prediction record of a key frame macroblock (11.2, 11.3).
fn read_key_frame_modes(bd: &mut BoolDecoder, above_bmodes: &[u8; 4], left_bmodes: &[u8; 4]) -> MbModes {
    let mut modes = MbModes::default();
    modes.info.mode = bd.read_tree(&KF_YMODE_TREE, &KF_YMODE_PROBS);
    if modes.info.mode == B_PRED {
        for b in 0..16 {
            let a = if b < 4 { above_bmodes[b] } else { modes.bmodes[b - 4] };
            let l = if b & 3 == 0 { left_bmodes[b >> 2] } else { modes.bmodes[b - 1] };
            let o = (usize::from(a) * 10 + usize::from(l)) * 9;
            modes.bmodes[b] = bd.read_tree(&BMODE_TREE, &KF_BMODE_PROBS[o..o + 9]);
        }
    } else {
        modes.bmodes = [implied_bmode(modes.info.mode); 16];
    }
    modes.uv_mode = bd.read_tree(&UV_MODE_TREE, &KF_UV_MODE_PROBS);
    modes
}

/// Non-zero context update for a macroblock without coefficients: everything is reset, except
/// the Y2 flags when the macroblock has no Y2 block.
fn skip_residual(has_y2: bool, above_nz: &mut [u8; 9], left_nz: &mut [u8; 9]) {
    let keep_y2 = if has_y2 { 0 } else { left_nz[8] };
    let keep_y2_above = if has_y2 { 0 } else { above_nz[8] };
    *left_nz = [0; 9];
    *above_nz = [0; 9];
    left_nz[8] = keep_y2;
    above_nz[8] = keep_y2_above;
}

/// Tokens of all blocks of one macroblock (13), updating the non-zero contexts.
fn read_residual(
    td: &mut BoolDecoder,
    coeff_probs: &[[[[u8; 11]; 3]; 8]; 4],
    dq: &Dequant,
    has_y2: bool,
    an: &mut [u8; 9],
    left_nz: &mut [u8; 9],
) -> MbResidual {
    let mut r = MbResidual::default();
    let first = if has_y2 {
        let ctx = usize::from(an[8] + left_nz[8]);
        let eob = read_coefficients(td, &coeff_probs[1], ctx, 0, dq.y2, &mut r.coeffs[24]);
        let nz = u8::from(eob > 0);
        an[8] = nz;
        left_nz[8] = nz;
        r.nonzero[24] = eob > 0;
        r.has_coeffs |= eob > 0;
        1
    } else {
        0
    };
    let y_probs = if has_y2 { &coeff_probs[0] } else { &coeff_probs[3] };
    for b in 0..16 {
        let (bx, by) = (b & 3, b >> 2);
        let ctx = usize::from(an[bx] + left_nz[by]);
        let eob = read_coefficients(td, y_probs, ctx, first, dq.y, &mut r.coeffs[b]);
        let nz = eob > first;
        an[bx] = u8::from(nz);
        left_nz[by] = u8::from(nz);
        r.nonzero[b] = nz;
        r.has_coeffs |= nz;
    }
    for b in 16..24 {
        let (off, k) = if b < 20 { (4, b - 16) } else { (6, b - 20) };
        let (bx, by) = (k & 1, k >> 1);
        let ctx = usize::from(an[off + bx] + left_nz[off + by]);
        let eob = read_coefficients(td, &coeff_probs[2], ctx, 0, dq.uv, &mut r.coeffs[b]);
        let nz = eob > 0;
        an[off + bx] = u8::from(nz);
        left_nz[off + by] = u8::from(nz);
        r.nonzero[b] = nz;
        r.has_coeffs |= nz;
    }
    if has_y2 && r.nonzero[24] {
        let dc = transform::inverse_wht(&r.coeffs[24]);
        for b in 0..16 {
            r.coeffs[b][0] = dc[b];
            r.nonzero[b] |= dc[b] != 0;
        }
    }
    r
}

/// Prediction plus residue of one macroblock into `frame`.
fn reconstruct_mb(frame: &mut Frame, (mb_x, mb_y): (usize, usize), mb_w: usize, modes: &MbModes, res: &MbResidual, recon: &InterRecon) -> Result<()> {
    let info = &modes.info;
    let (px, py) = (mb_x * 16, mb_y * 16);
    let ys = frame.w;
    let cs = frame.w / 2;
    if info.ref_frame == 0 {
        if info.mode == B_PRED {
            for b in 0..16 {
                predict::predict_subblock(&mut frame.y, ys, px, py, b, modes.bmodes[b], mb_x + 1 == mb_w);
                if res.nonzero[b] {
                    let pos = (py + 4 * (b >> 2)) * ys + px + 4 * (b & 3);
                    transform::add_residue(&mut frame.y, ys, pos, &transform::inverse_dct(&res.coeffs[b]));
                }
            }
        } else {
            predict::predict_block(&mut frame.y, ys, px, py, 16, info.mode);
            add_luma_residue(&mut frame.y, ys, px, py, &res.coeffs, &res.nonzero);
        }
        predict::predict_block(&mut frame.u, cs, px / 2, py / 2, 8, modes.uv_mode);
        predict::predict_block(&mut frame.v, cs, px / 2, py / 2, 8, modes.uv_mode);
    } else {
        let Some(r) = recon.refs.get(usize::from(info.ref_frame) - 1).and_then(Option::as_deref) else {
            return Err(Error::MissingReference(format!("reference frame {}", info.ref_frame)));
        };
        predict_inter_mb(r, frame, mb_x, mb_y, info, recon.filters, recon.full_pixel);
        add_luma_residue(&mut frame.y, ys, px, py, &res.coeffs, &res.nonzero);
    }
    for b in 16..24 {
        if res.nonzero[b] {
            let k = (b - 16) & 3;
            let pos = (py / 2 + 4 * (k >> 1)) * cs + px / 2 + 4 * (k & 1);
            let plane = if b < 20 { &mut frame.u } else { &mut frame.v };
            transform::add_residue(plane, cs, pos, &transform::inverse_dct(&res.coeffs[b]));
        }
    }
    Ok(())
}

/// `read_mvcomponent` (17.1), in quarter pixels.
fn read_mv_component(bd: &mut BoolDecoder, p: &[u8; 19]) -> i32 {
    let a = if bd.read_bool(p[MVP_IS_SHORT]) {
        let mut a = 0i32;
        for i in 0..3 {
            a += i32::from(bd.read_bool(p[MVP_BITS + i])) << i;
        }
        for i in (4..10).rev() {
            a += i32::from(bd.read_bool(p[MVP_BITS + i])) << i;
        }
        if a & 0xfff0 == 0 || bd.read_bool(p[MVP_BITS + 3]) {
            a += 8;
        }
        a
    } else {
        i32::from(bd.read_tree(&SMALL_MV_TREE, &p[MVP_SHORT..MVP_SHORT + 7]))
    };
    if a != 0 && bd.read_bool(p[MVP_SIGN]) { -a } else { a }
}

/// The limits `vp8_clamp_mv` (16.3) keeps "nearest", "near" and "best" within: 16 pixels
/// beyond the frame's macroblocks, in quarter pixels.
struct MvBounds {
    min_col: i32,
    max_col: i32,
    min_row: i32,
    max_row: i32,
}

impl MvBounds {
    fn new(mb_x: usize, mb_y: usize, mb_w: usize, mb_h: usize) -> MvBounds {
        const MARGIN: i32 = 16 << 2;
        MvBounds {
            min_col: -((mb_x as i32 * 16) << 2) - MARGIN,
            max_col: (((mb_w - 1 - mb_x) as i32 * 16) << 2) + MARGIN,
            min_row: -((mb_y as i32 * 16) << 2) - MARGIN,
            max_row: (((mb_h - 1 - mb_y) as i32 * 16) << 2) + MARGIN,
        }
    }
    fn clamp(&self, m: Mv) -> Mv {
        Mv { row: m.row.clamp(self.min_row, self.max_row), col: m.col.clamp(self.min_col, self.max_col) }
    }
}

/// Subblock mode a 16x16 luma mode stands for in B_PRED contexts (11.3).
fn implied_bmode(mode: u8) -> u8 {
    match mode {
        V_PRED => B_VE_PRED,
        H_PRED => B_HE_PRED,
        TM_PRED => B_TM_PRED,
        _ => B_DC_PRED,
    }
}

/// Decode the tokens of one block (13), storing dequantized coefficients in raster order.
/// Returns the position after the last token (where the end of block was read, or 16).
fn read_coefficients(bd: &mut BoolDecoder, probs: &[[[u8; 11]; 3]; 8], ctx: usize, first: usize, dq: [i32; 2], out: &mut [i16; 16]) -> usize {
    let mut ctx = ctx.min(2);
    let mut prev_zero = false;
    let mut i = first;
    while i < 16 {
        let p = &probs[usize::from(COEFF_BANDS[i])][ctx];
        let token = if prev_zero { bd.read_tree_from(&COEFF_TREE, p, 2) } else { bd.read_tree(&COEFF_TREE, p) };
        if token == DCT_EOB {
            break;
        }
        let abs = match token {
            DCT_0..=DCT_4 => i32::from(token),
            _ => {
                let cat = usize::from(token - DCT_CAT1).min(5);
                let mut extra = 0i32;
                for &pb in PCAT[cat] {
                    extra = extra + extra + i32::from(bd.read_bool(pb));
                }
                CAT_BASE[cat] + extra
            }
        };
        let v = if abs != 0 && bd.read_bool(128) { -abs } else { abs };
        ctx = match abs {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        prev_zero = token == DCT_0;
        out[ZIGZAG[i]] = (v * dq[usize::from(i > 0)]) as i16;
        i += 1;
    }
    i
}

/// Add the residue of the 16 Y subblocks of the macroblock at `(px, py)`.
fn add_luma_residue(plane: &mut [u8], stride: usize, px: usize, py: usize, coeffs: &[[i16; 16]; 25], nonzero: &[bool; 25]) {
    for b in 0..16 {
        if nonzero[b] {
            let pos = (py + 4 * (b >> 2)) * stride + px + 4 * (b & 3);
            transform::add_residue(plane, stride, pos, &transform::inverse_dct(&coeffs[b]));
        }
    }
}

/// Inter prediction of a macroblock (18) into `frame`.
fn predict_inter_mb(r: &Frame, frame: &mut Frame, mb_x: usize, mb_y: usize, info: &MbInfo, filters: &[i32; 48], full_pixel: bool) {
    let (px, py) = (mb_x * 16, mb_y * 16);
    let ys = frame.w;
    let cs = frame.w / 2;
    let ry = RefPlane { data: &r.y, stride: r.w, width: r.w, height: r.h };
    let ru = RefPlane { data: &r.u, stride: r.w / 2, width: r.w / 2, height: r.h / 2 };
    let rv = RefPlane { data: &r.v, stride: r.w / 2, width: r.w / 2, height: r.h / 2 };
    let fp = |v: i32| if full_pixel { v & !7 } else { v };
    if info.mode != MV_SPLIT {
        let m = info.mv;
        predict::predict_inter(&ry, DstPlane { data: &mut frame.y, stride: ys }, BlockRect { x: px, y: py, w: 16, h: 16 }, (m.col * 2, m.row * 2), filters);
        // chroma: the luma vector in quarter pixels is the chroma vector in eighth pixels
        let (cx, cy) = (fp(m.col), fp(m.row));
        predict::predict_inter(&ru, DstPlane { data: &mut frame.u, stride: cs }, BlockRect { x: px / 2, y: py / 2, w: 8, h: 8 }, (cx, cy), filters);
        predict::predict_inter(&rv, DstPlane { data: &mut frame.v, stride: cs }, BlockRect { x: px / 2, y: py / 2, w: 8, h: 8 }, (cx, cy), filters);
        return;
    }
    for b in 0..16 {
        let m = info.bmv[b];
        let rect = BlockRect { x: px + 4 * (b & 3), y: py + 4 * (b >> 2), w: 4, h: 4 };
        predict::predict_inter(&ry, DstPlane { data: &mut frame.y, stride: ys }, rect, (m.col * 2, m.row * 2), filters);
    }
    for i in 0..2 {
        for j in 0..2 {
            let k = 8 * i + 2 * j;
            let sum = |f: fn(Mv) -> i32| [k, k + 1, k + 4, k + 5].iter().map(|&b| f(info.bmv[b])).sum::<i32>();
            // average of the four luma vectors, in eighth chroma pixels (18.1)
            let avg = |s: i32| (s + 2 + if s < 0 { -1 } else { 0 }) >> 2;
            let cx = fp(avg(sum(|m| m.col)));
            let cy = fp(avg(sum(|m| m.row)));
            let (bx, by) = (px / 2 + 4 * j, py / 2 + 4 * i);
            let rect = BlockRect { x: bx, y: by, w: 4, h: 4 };
            predict::predict_inter(&ru, DstPlane { data: &mut frame.u, stride: cs }, rect, (cx, cy), filters);
            predict::predict_inter(&rv, DstPlane { data: &mut frame.v, stride: cs }, rect, (cx, cy), filters);
        }
    }
}

/// Loop filter of the whole frame (15.1), macroblocks in raster order.
fn loop_filter(frame: &mut Frame, mb_w: usize, mb_h: usize, lf_info: &[(u8, bool)], hdr: &FrameHeader, key_frame: bool) {
    let ys = frame.w;
    let cs = frame.w / 2;
    let simple = hdr.filter_type == 1;
    for mb_y in 0..mb_h {
        for mb_x in 0..mb_w {
            let Some(&(level, inner)) = lf_info.get(mb_y * mb_w + mb_x) else { continue };
            if level == 0 {
                continue;
            }
            let prm = Params::new(level, hdr.sharpness, key_frame);
            let y0 = mb_y * 16 * ys + mb_x * 16;
            let c0 = mb_y * 8 * cs + mb_x * 8;
            if simple {
                if mb_x > 0 {
                    loopfilter::filter_edge::<16>(&mut frame.y, ys, y0, true, Kind::Simple, &prm);
                }
                if inner {
                    for x in [4, 8, 12] {
                        loopfilter::filter_edge::<16>(&mut frame.y, ys, y0 + x, true, Kind::SimpleInner, &prm);
                    }
                }
                if mb_y > 0 {
                    loopfilter::filter_edge::<16>(&mut frame.y, ys, y0, false, Kind::Simple, &prm);
                }
                if inner {
                    for y in [4, 8, 12] {
                        loopfilter::filter_edge::<16>(&mut frame.y, ys, y0 + y * ys, false, Kind::SimpleInner, &prm);
                    }
                }
                continue;
            }
            if mb_x > 0 {
                loopfilter::filter_edge::<16>(&mut frame.y, ys, y0, true, Kind::NormalMb, &prm);
                loopfilter::filter_edge::<8>(&mut frame.u, cs, c0, true, Kind::NormalMb, &prm);
                loopfilter::filter_edge::<8>(&mut frame.v, cs, c0, true, Kind::NormalMb, &prm);
            }
            if inner {
                for x in [4, 8, 12] {
                    loopfilter::filter_edge::<16>(&mut frame.y, ys, y0 + x, true, Kind::NormalSub, &prm);
                }
                loopfilter::filter_edge::<8>(&mut frame.u, cs, c0 + 4, true, Kind::NormalSub, &prm);
                loopfilter::filter_edge::<8>(&mut frame.v, cs, c0 + 4, true, Kind::NormalSub, &prm);
            }
            if mb_y > 0 {
                loopfilter::filter_edge::<16>(&mut frame.y, ys, y0, false, Kind::NormalMb, &prm);
                loopfilter::filter_edge::<8>(&mut frame.u, cs, c0, false, Kind::NormalMb, &prm);
                loopfilter::filter_edge::<8>(&mut frame.v, cs, c0, false, Kind::NormalMb, &prm);
            }
            if inner {
                for y in [4, 8, 12] {
                    loopfilter::filter_edge::<16>(&mut frame.y, ys, y0 + y * ys, false, Kind::NormalSub, &prm);
                }
                loopfilter::filter_edge::<8>(&mut frame.u, cs, c0 + 4 * cs, false, Kind::NormalSub, &prm);
                loopfilter::filter_edge::<8>(&mut frame.v, cs, c0 + 4 * cs, false, Kind::NormalSub, &prm);
            }
        }
    }
}
