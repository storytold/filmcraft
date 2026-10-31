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
    ABOVE_4X4, B_DC_PRED, B_HE_PRED, B_PRED, B_TM_PRED, B_VE_PRED, BMODE_TREE, CAT_BASE, DC_PRED, H_PRED, KF_YMODE_TREE, LEFT_4X4, MV_NEAR, MV_NEAREST, MV_NEW,
    MV_REF_TREE, MV_SPLIT, MV_ZERO, MVP_BITS, MVP_IS_SHORT, MVP_SHORT, MVP_SIGN, MVPARTITION_TREE, PCAT, SEGMENT_TREE, SMALL_MV_TREE, SPLIT_COUNTS, SPLIT_MAPS,
    SUB_MV_REF_TREE, TM_PRED, UV_MODE_TREE, V_PRED, YMODE_TREE, ZERO_4X4, ZIGZAG,
};
use crate::transform;

#[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
mod frames;

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
    /// A key frame was decoded since the start or the last reset (inter frames need one).
    key_seen: bool,
    /// The frames on worker threads, with more than one ([`frames`]); `refs` is then theirs.
    #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
    frames: Option<frames::Pipeline>,
}

/// Threads of [`Decoder::new`]: the available cores up to 16 with the `threads` feature, else 1.
pub fn default_threads() -> usize {
    #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
    {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(16)
    }
    #[cfg(not(all(feature = "threads", not(target_arch = "wasm32"))))]
    {
        1
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    /// A decoder using all available cores (with the `threads` feature).
    pub fn new() -> Decoder {
        Self::with_threads(default_threads())
    }

    /// A decoder for `threads` threads (1: everything on the calling thread, each picture
    /// returned by the [`Decoder::decode`] call of its frame). With more, the frames go through a
    /// pipeline of worker threads: a token reader per frame read at once (`threads - 4`, at least
    /// half of `threads`) and 4 for the planes, so at least 5, and a picture may come out of a
    /// later call or [`Decoder::flush`]. The pictures are the same for every thread count.
    pub fn with_threads(threads: usize) -> Decoder {
        #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
        let frames = (threads > 1).then(|| frames::Pipeline::new(threads)).flatten();
        #[cfg(not(all(feature = "threads", not(target_arch = "wasm32"))))]
        let _ = threads;
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
            key_seen: false,
            #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
            frames,
        }
    }

    /// Threads in use (1: everything runs on the calling thread).
    pub fn threads(&self) -> usize {
        #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
        if let Some(frames) = &self.frames {
            return frames.threads();
        }
        1
    }

    /// Forget the reference frames and the pictures in flight (after a seek): decoding resumes
    /// at the next key frame.
    pub fn reset(&mut self) {
        self.refs = [None, None, None];
        self.key_seen = false;
        #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
        if let Some(frames) = self.frames.take() {
            self.frames = frames::Pipeline::new(frames.threads());
        }
    }

    /// End of stream: every picture still in flight, in order.
    pub fn flush(&mut self) -> Vec<Picture> {
        #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
        if let Some(frames) = &mut self.frames {
            return frames.drain(true);
        }
        Vec::new()
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
        if tag.key_frame || !self.key_seen {
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

    /// Prediction record of the macroblock at (`mb_x`, `mb_y`) (first partition): its segment
    /// (updating `seg_map`), skip flag and modes, kept in `mc` for the macroblocks after it.
    #[inline(always)]
    fn read_mb_header(&self, bd: &mut BoolDecoder, f: &FrameParams, seg_map: &mut [u8], mc: &mut ModeCtx, (mb_x, mb_y): (usize, usize)) -> MbRec {
        let pos = MbPos { idx: (mb_y + 1) * mc.stride + mb_x + 1, stride: mc.stride, mb_x, mb_y };
        let segment = read_segment(&self.seg, seg_map, bd, mb_y * self.mb_w + mb_x);
        let skip = f.hdr.skip_prob.is_some_and(|p| bd.read_bool(p));
        let modes = if f.key_frame {
            read_key_frame_modes(bd, &mc.above_bmodes[mb_x], &mc.left_bmodes)
        } else {
            self.read_inter_frame_modes(bd, &f.hdr, &mc.infos, pos)
        };
        mc.above_bmodes[mb_x] = [modes.bmodes[12], modes.bmodes[13], modes.bmodes[14], modes.bmodes[15]];
        mc.left_bmodes = [modes.bmodes[3], modes.bmodes[7], modes.bmodes[11], modes.bmodes[15]];
        if let Some(slot) = mc.infos.get_mut(pos.idx) {
            *slot = modes.info;
        }
        MbRec { modes, segment, skip }
    }

    /// The prediction records of every macroblock of a frame (the first partition), by row.
    #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
    fn read_headers(&self, bd: &mut BoolDecoder, f: &FrameParams, seg_map: &mut [u8]) -> Vec<Vec<MbRec>> {
        let mut mc = ModeCtx::new(self.mb_w, self.mb_h);
        (0..self.mb_h)
            .map(|mb_y| {
                mc.left_bmodes = [B_DC_PRED; 4];
                (0..self.mb_w).map(|mb_x| self.read_mb_header(bd, f, seg_map, &mut mc, (mb_x, mb_y))).collect()
            })
            .collect()
    }

    /// The macroblocks of a frame on the calling thread: each row's prediction records, tokens
    /// and reconstruction, then the loop filter of the row above.
    fn decode_rows(&self, bd: &mut BoolDecoder, parts: &mut [BoolDecoder], f: &FrameParams, recon: &InterRecon, seg_map: &mut [u8]) -> Result<Frame> {
        let (mb_w, mb_h) = (self.mb_w, self.mb_h);
        let mut frame = Frame::new(mb_w * 16, mb_h * 16);
        let mut mc = ModeCtx::new(mb_w, mb_h);
        let mut above_nz = vec![[0u8; 9]; mb_w];
        // The dequantized coefficients of a macroblock: zero between macroblocks (each clears
        // the blocks it wrote).
        let mut residual = MbResidual::default();
        // Loop filter level and inner edge flag of each macroblock of this row and the one above.
        let mut lf = [vec![(0u8, false); mb_w], vec![(0u8, false); mb_w]];
        let filter = f.hdr.filter_level > 0;
        for mb_y in 0..mb_h {
            let mut left_nz = [0u8; 9];
            mc.left_bmodes = [B_DC_PRED; 4];
            let td = &mut parts[mb_y % parts.len()];
            for mb_x in 0..mb_w {
                let rec = self.read_mb_header(bd, f, seg_map, &mut mc, (mb_x, mb_y));
                read_mb_tokens(td, f, &self.ctx.coeff, &rec, (&mut above_nz[mb_x], &mut left_nz), &mut residual);
                if let Some(l) = lf[0].get_mut(mb_x) {
                    *l = (filter_level(f, rec.segment, &rec.modes.info), !rec.has_y2() || residual.has_coeffs);
                }
                reconstruct_mb::<true, true>(&mut frame, (mb_x, mb_y), 0, mb_w, &rec.modes, &residual, recon)?;
                residual.clear();
            }
            if filter && mb_y > 0 {
                loop_filter_row::<true, true>(&mut frame, mb_y - 1, &lf[1], &f.hdr, f.key_frame);
            }
            lf.swap(0, 1);
        }
        if filter && mb_h > 0 {
            loop_filter_row::<true, true>(&mut frame, mb_h - 1, &lf[1], &f.hdr, f.key_frame);
        }
        Ok(frame)
    }

    /// Prediction record of a macroblock of an inter frame (11.2, 16).
    #[inline(always)]
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

    /// Decode one compressed frame. Returns the pictures finished so far, in order: with one
    /// thread, this frame's when it is meant to be shown (`show_frame`; none for a hidden
    /// altref); with more, a picture may come out of a later call or [`Decoder::flush`].
    pub fn decode(&mut self, data: &[u8], pts: i64) -> Result<Vec<Picture>> {
        let tag = FrameTag::parse(data)?;
        ensure!(tag.first_part_size > 0, "empty first partition");
        if tag.key_frame {
            self.begin_key_frame(&tag)?;
        } else if !self.key_seen {
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
        let parts = token_partitions(data, first_end, hdr.partitions);
        let f = FrameParams {
            dequant: self.segment_dequant(&hdr),
            hdr,
            key_frame: tag.key_frame,
            seg: self.seg,
            lf: self.lf,
            filters: if tag.version == 0 || tag.version > 3 { &SIXTAP_FILTERS } else { &BILINEAR_FILTERS },
            full_pixel: tag.version == 3,
        };
        let mut seg_map = std::mem::take(&mut self.seg_map);

        #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
        if let Some(mut frames) = self.frames.take() {
            // The first partition here (the next frame's segment map and probabilities depend on
            // it), the rest on the worker threads.
            let recs = self.read_headers(&mut bd, &f, &mut seg_map);
            self.seg_map = seg_map;
            let coeff = Box::new(self.ctx.coeff);
            if let Some(s) = saved {
                self.ctx = s;
            }
            self.count(&tag);
            let job = frames::Job {
                f,
                coeff,
                data: data.into(),
                parts,
                recs,
                size: (self.width, self.height),
                mb: (self.mb_w, self.mb_h),
                pts,
                show: tag.show_frame,
            };
            let out = frames.submit(job);
            self.frames = Some(frames);
            return out;
        }

        let mut parts: Vec<BoolDecoder> = parts.iter().map(|r| BoolDecoder::new(data.get(r.clone()).unwrap_or(&[]))).collect();
        let recon = InterRecon { refs: self.refs.clone(), filters: f.filters, full_pixel: f.full_pixel };
        let frame = self.decode_rows(&mut bd, &mut parts, &f, &recon, &mut seg_map);
        self.seg_map = seg_map;
        let frame = Arc::new(frame?);
        if let Some(s) = saved {
            self.ctx = s;
        }
        self.refs = updated_refs(&self.refs, &f.hdr, &frame);
        self.count(&tag);
        Ok(if tag.show_frame { vec![picture(&frame, (self.width, self.height), pts, tag.key_frame, f.hdr.color_space)] } else { Vec::new() })
    }

    /// A frame decoded (or on its way through the worker threads).
    fn count(&mut self, tag: &FrameTag) {
        self.key_seen = true;
        self.stats.frames += 1;
        self.stats.key_frames += u64::from(tag.key_frame);
        self.stats.hidden_frames += u64::from(!tag.show_frame);
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
#[derive(Clone)]
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

/// What the first partition says about one macroblock: its modes, segment and whether it has
/// no coefficients.
#[derive(Clone)]
struct MbRec {
    modes: MbModes,
    segment: usize,
    skip: bool,
}

impl MbRec {
    /// The macroblock has a Y2 block (13): every mode but B_PRED and SPLITMV.
    fn has_y2(&self) -> bool {
        self.modes.info.mode != B_PRED && self.modes.info.mode != MV_SPLIT
    }
}

/// What reading the prediction records carries from macroblock to macroblock: the records read
/// so far (with a border row and column, row stride `stride`, for motion vector prediction) and
/// the B_PRED contexts above and left (key frames).
struct ModeCtx {
    infos: Vec<MbInfo>,
    stride: usize,
    above_bmodes: Vec<[u8; 4]>,
    left_bmodes: [u8; 4],
}

impl ModeCtx {
    fn new(mb_w: usize, mb_h: usize) -> ModeCtx {
        ModeCtx {
            infos: vec![MbInfo::default(); (mb_w + 1) * (mb_h + 1)],
            stride: mb_w + 1,
            above_bmodes: vec![[B_DC_PRED; 4]; mb_w],
            left_bmodes: [B_DC_PRED; 4],
        }
    }
}

/// What decoding the macroblocks of a frame reads besides its data and the reference frames:
/// its header, the dequantization factors of each segment, the segmentation and loop filter
/// delta settings (for the loop filter levels) and the inter prediction filters.
struct FrameParams {
    hdr: FrameHeader,
    key_frame: bool,
    dequant: [Dequant; 4],
    seg: Segmentation,
    lf: LfDeltas,
    filters: &'static [i32; 48],
    full_pixel: bool,
}

/// The coefficient probabilities (13.4): by block type, band, context and token tree node.
type CoeffProbs = [[[[u8; 11]; 3]; 8]; 4];

/// The last, golden and altref frames.
type Refs = [Option<Arc<Frame>>; 3];

/// Loop filter level of a macroblock (15.1 with the adjustments of 9.3 and 9.4).
fn filter_level(f: &FrameParams, segment: usize, info: &MbInfo) -> u8 {
    let mut level = i32::from(f.hdr.filter_level);
    if f.seg.enabled {
        let v = i32::from(f.seg.lf_level[segment]);
        level = if f.seg.absolute { v } else { level + v }.clamp(0, 63);
    }
    if f.lf.enabled {
        level += i32::from(f.lf.ref_deltas[usize::from(info.ref_frame)]);
        let mode_delta = match (info.ref_frame, info.mode) {
            (0, B_PRED) => Some(0),
            (0, _) => None,
            (_, MV_ZERO) => Some(1),
            (_, MV_SPLIT) => Some(3),
            _ => Some(2),
        };
        if let Some(m) = mode_delta {
            level += i32::from(f.lf.mode_deltas[m]);
        }
        level = level.clamp(0, 63);
    }
    level as u8
}

/// Reference updates (9.7, 9.8) of `refs` by a frame with header `hdr`: the copies first
/// (altref, then golden from the possibly updated altref), then the refreshes with the frame.
fn updated_refs(refs: &Refs, hdr: &FrameHeader, frame: &Arc<Frame>) -> Refs {
    let [last, golden, altref] = refs.clone();
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
    [new_last, new_golden, new_altref]
}

/// The visible `size` part of a reconstructed frame.
fn picture(f: &Frame, (w, h): (usize, usize), pts: i64, key: bool, color_space: u8) -> Picture {
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

/// Reads the segment id (when the map is updated) into `seg_map` and returns the segment of
/// the macroblock.
#[inline(always)]
fn read_segment(seg: &Segmentation, seg_map: &mut [u8], bd: &mut BoolDecoder, seg_idx: usize) -> usize {
    if seg.update_map {
        let s = bd.read_tree(&SEGMENT_TREE, &seg.tree_probs);
        if let Some(m) = seg_map.get_mut(seg_idx) {
            *m = s;
        }
    }
    if seg.enabled { seg_map.get(seg_idx).copied().unwrap_or(0) as usize & 3 } else { 0 }
}

/// Tokens of one macroblock (13) into `res` (cleared), or the context update of a macroblock
/// without coefficients; `nz` are the non-zero contexts above and left.
#[inline(always)]
fn read_mb_tokens(td: &mut BoolDecoder, f: &FrameParams, coeff: &CoeffProbs, rec: &MbRec, (an, ln): (&mut [u8; 9], &mut [u8; 9]), res: &mut MbResidual) {
    if rec.skip {
        skip_residual(rec.has_y2(), an, ln);
    } else {
        read_residual(td, coeff, &f.dequant[rec.segment], rec.has_y2(), an, ln, res);
    }
}

/// Dequantized coefficients of one macroblock: 16 Y, 4 U, 4 V and the Y2 block. At the start of
/// a 128-byte cache line (the line of Apple cores): wherever the stack put it, which moves with
/// the size of the process environment, the frame loop ran up to 15% slower.
#[derive(Default)]
#[repr(align(128))]
struct MbResidual {
    coeffs: [[i16; 16]; 25],
    nonzero: [bool; 25],
    has_coeffs: bool,
}

impl MbResidual {
    /// Back to zero for the next macroblock: only blocks with coefficients (tokens past the
    /// first position, or the Y2 DC) were written.
    fn clear(&mut self) {
        for (c, _) in self.coeffs.iter_mut().zip(self.nonzero).filter(|(_, nz)| *nz) {
            *c = [0; 16];
        }
        self.nonzero = [false; 25];
        self.has_coeffs = false;
    }

    /// [`MbResidual::clear`] when the blocks with coefficients are known: `blocks` (bit b: block
    /// b).
    #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
    fn clear_blocks(&mut self, blocks: u32) {
        let mut left = blocks;
        while left != 0 {
            let b = left.trailing_zeros() as usize;
            left &= left - 1;
            if let (Some(c), Some(nz)) = (self.coeffs.get_mut(b), self.nonzero.get_mut(b)) {
                *c = [0; 16];
                *nz = false;
            }
        }
        self.has_coeffs = false;
    }
}

/// What inter prediction of a frame needs: the reference frames and the sub-pixel filter setup.
struct InterRecon {
    refs: Refs,
    filters: &'static [i32; 48],
    full_pixel: bool,
}

/// Where in `data` the token partitions (9.5) are: sizes of all but the last, then the data.
fn token_partitions(data: &[u8], sizes_start: usize, n: usize) -> Vec<std::ops::Range<usize>> {
    let mut pos = sizes_start.saturating_add(3 * (n - 1)).min(data.len());
    let mut parts = Vec::with_capacity(n);
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
        parts.push(pos..end);
        pos = end;
    }
    parts
}

/// Prediction record of a key frame macroblock (11.2, 11.3).
#[inline(always)]
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
#[inline(always)]
fn skip_residual(has_y2: bool, above_nz: &mut [u8; 9], left_nz: &mut [u8; 9]) {
    let keep_y2 = if has_y2 { 0 } else { left_nz[8] };
    let keep_y2_above = if has_y2 { 0 } else { above_nz[8] };
    *left_nz = [0; 9];
    *above_nz = [0; 9];
    left_nz[8] = keep_y2;
    above_nz[8] = keep_y2_above;
}

/// Tokens of all blocks of one macroblock (13) into `r` (cleared), updating the non-zero
/// contexts.
#[inline(always)]
fn read_residual(
    td: &mut BoolDecoder,
    coeff_probs: &[[[[u8; 11]; 3]; 8]; 4],
    dq: &Dequant,
    has_y2: bool,
    an: &mut [u8; 9],
    left_nz: &mut [u8; 9],
    r: &mut MbResidual,
) {
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
}

/// Prediction plus residue of the macroblock at (`mb_x`, `mb_y`) into `frame` (of its luma plane
/// with `LUMA`, of its chroma planes with `CHROMA`: they are independent), which holds the
/// frame's luma rows from `top` on (a multiple of 16; chroma from `top / 2`): 0 for the whole
/// frame, or the row of macroblocks above for a buffer of two rows whose upper one has only its
/// last line, the line intra prediction reads.
#[inline(always)]
fn reconstruct_mb<const LUMA: bool, const CHROMA: bool>(
    frame: &mut Frame,
    (mb_x, mb_y): (usize, usize),
    top: usize,
    mb_w: usize,
    modes: &MbModes,
    res: &MbResidual,
    recon: &InterRecon,
) -> Result<()> {
    let info = &modes.info;
    // Intra prediction, which reads the frame's first row as the top row, and the residue go by
    // the buffer's rows; inter prediction reads the reference at the frame position.
    let (px, py) = (mb_x * 16, (mb_y * 16).saturating_sub(top));
    let ys = frame.w;
    let cs = frame.w / 2;
    if info.ref_frame == 0 {
        if LUMA && info.mode == B_PRED {
            for b in 0..16 {
                predict::predict_subblock(&mut frame.y, ys, px, py, b, modes.bmodes[b], mb_x + 1 == mb_w);
                if res.nonzero[b] {
                    let pos = (py + 4 * (b >> 2)) * ys + px + 4 * (b & 3);
                    transform::idct_add(&mut frame.y, ys, pos, &res.coeffs[b]);
                }
            }
        } else if LUMA {
            predict::predict_block(&mut frame.y, ys, px, py, 16, info.mode);
            add_luma_residue(&mut frame.y, ys, px, py, &res.coeffs, &res.nonzero);
        }
        if CHROMA {
            predict::predict_block(&mut frame.u, cs, px / 2, py / 2, 8, modes.uv_mode);
            predict::predict_block(&mut frame.v, cs, px / 2, py / 2, 8, modes.uv_mode);
        }
    } else {
        let Some(r) = recon.refs.get(usize::from(info.ref_frame) - 1).and_then(Option::as_deref) else {
            return Err(Error::MissingReference(format!("reference frame {}", info.ref_frame)));
        };
        predict_inter_mb::<LUMA, CHROMA>(r, frame, (mb_x, mb_y), top, info, recon.filters, recon.full_pixel);
        if LUMA {
            add_luma_residue(&mut frame.y, ys, px, py, &res.coeffs, &res.nonzero);
        }
    }
    if !CHROMA {
        return Ok(());
    }
    for b in 16..24 {
        if res.nonzero[b] {
            let k = (b - 16) & 3;
            let pos = (py / 2 + 4 * (k >> 1)) * cs + px / 2 + 4 * (k & 1);
            let plane = if b < 20 { &mut frame.u } else { &mut frame.v };
            transform::idct_add(plane, cs, pos, &res.coeffs[b]);
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
    // Through `&mut` the decoder state would go to memory after every bool; a local copy stays
    // in registers.
    let mut local = *bd;
    let d = &mut local;
    let mut ctx = ctx.min(2);
    let mut i = first;
    // coeff_tree (13.2) as branches: dct_eob, DCT_0 and DCT_1 first, the rarer tokens after.
    'tokens: while i < 16 {
        let mut p = &probs[usize::from(COEFF_BANDS[i])][ctx];
        if !d.read_bool(p[0]) {
            break; // dct_eob
        }
        // A run of DCT_0; no dct_eob may follow a zero, so the next token starts at node 2.
        while !d.read_bool(p[1]) {
            i += 1;
            if i == 16 {
                break 'tokens;
            }
            p = &probs[usize::from(COEFF_BANDS[i])][0];
        }
        let abs = if !d.read_bool(p[2]) {
            ctx = 1;
            1
        } else {
            ctx = 2;
            if !d.read_bool(p[3]) {
                if !d.read_bool(p[4]) {
                    2
                } else if !d.read_bool(p[5]) {
                    3
                } else {
                    4
                }
            } else {
                let cat = if !d.read_bool(p[6]) {
                    usize::from(d.read_bool(p[7]))
                } else if !d.read_bool(p[8]) {
                    2 + usize::from(d.read_bool(p[9]))
                } else {
                    4 + usize::from(d.read_bool(p[10]))
                };
                // dct_cat1 .. dct_cat6: the extra bits at their fixed probabilities
                let mut extra = 0i32;
                for &pb in PCAT[cat] {
                    extra = extra + extra + i32::from(d.read_bool(pb));
                }
                CAT_BASE[cat] + extra
            }
        };
        let v = if d.read_half() { -abs } else { abs };
        out[ZIGZAG[i]] = (v * dq[usize::from(i > 0)]) as i16;
        i += 1;
    }
    *bd = local;
    i
}

/// Add the residue of the 16 Y subblocks of the macroblock at `(px, py)`.
fn add_luma_residue(plane: &mut [u8], stride: usize, px: usize, py: usize, coeffs: &[[i16; 16]; 25], nonzero: &[bool; 25]) {
    for b in 0..16 {
        if nonzero[b] {
            let pos = (py + 4 * (b >> 2)) * stride + px + 4 * (b & 3);
            transform::idct_add(plane, stride, pos, &coeffs[b]);
        }
    }
}

/// Inter prediction of a macroblock (18) into `frame` (which holds the frame's rows from `top` on).
fn predict_inter_mb<const LUMA: bool, const CHROMA: bool>(
    r: &Frame,
    frame: &mut Frame,
    (mb_x, mb_y): (usize, usize),
    top: usize,
    info: &MbInfo,
    filters: &[i32; 48],
    full_pixel: bool,
) {
    let (px, py) = (mb_x * 16, mb_y * 16);
    let ys = frame.w;
    let cs = frame.w / 2;
    let ry = RefPlane { data: &r.y, stride: r.w, width: r.w, height: r.h };
    let ru = RefPlane { data: &r.u, stride: r.w / 2, width: r.w / 2, height: r.h / 2 };
    let rv = RefPlane { data: &r.v, stride: r.w / 2, width: r.w / 2, height: r.h / 2 };
    let fp = |v: i32| if full_pixel { v & !7 } else { v };
    if info.mode != MV_SPLIT {
        let m = info.mv;
        if LUMA {
            predict::predict_inter(
                &ry,
                DstPlane { data: &mut frame.y, stride: ys, top },
                BlockRect { x: px, y: py, w: 16, h: 16 },
                (m.col * 2, m.row * 2),
                filters,
            );
        }
        if !CHROMA {
            return;
        }
        // chroma: the luma vector in quarter pixels is the chroma vector in eighth pixels
        let (cx, cy) = (fp(m.col), fp(m.row));
        predict::predict_inter(
            &ru,
            DstPlane { data: &mut frame.u, stride: cs, top: top / 2 },
            BlockRect { x: px / 2, y: py / 2, w: 8, h: 8 },
            (cx, cy),
            filters,
        );
        predict::predict_inter(
            &rv,
            DstPlane { data: &mut frame.v, stride: cs, top: top / 2 },
            BlockRect { x: px / 2, y: py / 2, w: 8, h: 8 },
            (cx, cy),
            filters,
        );
        return;
    }
    for b in (0..16).filter(|_| LUMA) {
        let m = info.bmv[b];
        let rect = BlockRect { x: px + 4 * (b & 3), y: py + 4 * (b >> 2), w: 4, h: 4 };
        predict::predict_inter(&ry, DstPlane { data: &mut frame.y, stride: ys, top }, rect, (m.col * 2, m.row * 2), filters);
    }
    if !CHROMA {
        return;
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
            predict::predict_inter(&ru, DstPlane { data: &mut frame.u, stride: cs, top: top / 2 }, rect, (cx, cy), filters);
            predict::predict_inter(&rv, DstPlane { data: &mut frame.v, stride: cs, top: top / 2 }, rect, (cx, cy), filters);
        }
    }
}

/// Loop filter (15.1) of macroblock row `mb_y`, whose macroblocks have the levels and inner
/// edge flags `lf_row`; the rows are filtered in order, so the frame's macroblocks are in raster
/// order. The filter changes the 3 rows of samples above the row and the row itself, which no
/// intra prediction of a later row reads. `LUMA` / `CHROMA`: the planes filtered (each edge
/// filter reads and changes the samples of one plane, so they may be filtered apart). Together,
/// the planes of a macroblock are filtered in turn: luma alone, each horizontal edge loads what
/// the vertical ones just stored and waits for it.
fn loop_filter_row<const LUMA: bool, const CHROMA: bool>(frame: &mut Frame, mb_y: usize, lf_row: &[(u8, bool)], hdr: &FrameHeader, key_frame: bool) {
    for (mb_x, &lf) in lf_row.iter().enumerate() {
        loop_filter_mb::<LUMA, CHROMA>(frame, (mb_x, mb_y), lf, hdr, key_frame);
    }
}

/// [`loop_filter_row`] of rows `mb_y` and `mb_y + 1` (levels and inner edge flags `lf`)
/// together: each macroblock of the lower row after the one above and to the right of it in the
/// upper row (whose left edge reaches into it), the two rows taking turns. Their macroblocks
/// filtered in a row are two columns apart, changing samples far apart, so each pixel goes
/// through the same filters in the same order as in raster order; but one plane's horizontal
/// edges no longer wait for the samples its vertical edges just stored.
#[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
fn loop_filter_rows<const LUMA: bool, const CHROMA: bool>(frame: &mut Frame, mb_y: usize, lf: [&[(u8, bool)]; 2], hdr: &FrameHeader, key_frame: bool) {
    let [upper, lower] = lf;
    let mut ahead = upper.iter().enumerate();
    for (mb_x, &l) in lower.iter().enumerate() {
        for (ux, &u) in ahead.by_ref().take(if mb_x == 0 { 2 } else { 1 }) {
            loop_filter_mb::<LUMA, CHROMA>(frame, (ux, mb_y), u, hdr, key_frame);
        }
        loop_filter_mb::<LUMA, CHROMA>(frame, (mb_x, mb_y + 1), l, hdr, key_frame);
    }
    for (ux, &u) in ahead {
        loop_filter_mb::<LUMA, CHROMA>(frame, (ux, mb_y), u, hdr, key_frame);
    }
}

/// [`loop_filter_row`] of the macroblock at (`mb_x`, `mb_y`), with its loop filter level and
/// inner edge flag `(level, inner)`.
#[inline(always)]
fn loop_filter_mb<const LUMA: bool, const CHROMA: bool>(
    frame: &mut Frame,
    (mb_x, mb_y): (usize, usize),
    (level, inner): (u8, bool),
    hdr: &FrameHeader,
    key_frame: bool,
) {
    let ys = frame.w;
    let cs = frame.w / 2;
    let simple = hdr.filter_type == 1;
    if level == 0 {
        return;
    }
    let prm = Params::new(level, hdr.sharpness, key_frame);
    let y0 = mb_y * 16 * ys + mb_x * 16;
    let c0 = mb_y * 8 * cs + mb_x * 8;
    if simple {
        if !LUMA {
            // the simple filter leaves the chroma planes alone (15.2)
            return;
        }
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
        return;
    }
    if mb_x > 0 {
        if LUMA {
            loopfilter::filter_edge::<16>(&mut frame.y, ys, y0, true, Kind::NormalMb, &prm);
        }
        if CHROMA {
            loopfilter::filter_edge_uv(&mut frame.u, &mut frame.v, cs, c0, true, Kind::NormalMb, &prm);
        }
    }
    if inner {
        if LUMA {
            for x in [4, 8, 12] {
                loopfilter::filter_edge::<16>(&mut frame.y, ys, y0 + x, true, Kind::NormalSub, &prm);
            }
        }
        if CHROMA {
            loopfilter::filter_edge_uv(&mut frame.u, &mut frame.v, cs, c0 + 4, true, Kind::NormalSub, &prm);
        }
    }
    if mb_y > 0 {
        if LUMA {
            loopfilter::filter_edge::<16>(&mut frame.y, ys, y0, false, Kind::NormalMb, &prm);
        }
        if CHROMA {
            loopfilter::filter_edge_uv(&mut frame.u, &mut frame.v, cs, c0, false, Kind::NormalMb, &prm);
        }
    }
    if inner {
        if LUMA {
            for y in [4, 8, 12] {
                loopfilter::filter_edge::<16>(&mut frame.y, ys, y0 + y * ys, false, Kind::NormalSub, &prm);
            }
        }
        if CHROMA {
            loopfilter::filter_edge_uv(&mut frame.u, &mut frame.v, cs, c0 + 4 * cs, false, Kind::NormalSub, &prm);
        }
    }
}

#[cfg(test)]
mod token_tests {
    use super::*;
    use crate::tables::{COEFF_TREE, DCT_0, DCT_4, DCT_CAT1, DCT_EOB};

    /// The token loop as the specification's tree walk (13.2, `treed_read` with `coeff_tree`),
    /// the reference for [`read_coefficients`].
    fn reference(bd: &mut BoolDecoder, probs: &[[[u8; 11]; 3]; 8], ctx: usize, first: usize, dq: [i32; 2], out: &mut [i16; 16]) -> usize {
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

    /// The branch-coded token loop reads what the tree walk reads (coefficients, end of block, and
    /// where the bool decoder stands afterwards) for random data, probabilities from very skewed
    /// to even, every context and first position.
    #[test]
    fn branch_coded_tokens_match_the_tree_walk() {
        let mut seed = 0x5eed_c0ef_f1c1_e575u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for case in 0..3000 {
            let data: Vec<u8> = (0..64).map(|_| next() as u8).collect();
            let skew = [1u64, 30, 128, 225, 254][case % 5];
            let mut probs = [[[0u8; 11]; 3]; 8];
            for p in probs.iter_mut().flatten().flatten() {
                *p = (skew + next() % 31).clamp(1, 255) as u8;
            }
            let (ctx, first) = ((next() % 3) as usize, (next() % 2) as usize);
            let dq = [(next() % 150) as i32 + 1, (next() % 150) as i32 + 1];
            let (mut a, mut b) = (BoolDecoder::new(&data), BoolDecoder::new(&data));
            // several blocks in a row, as a partition holds them
            for blk in 0..4 {
                let (mut oa, mut ob) = ([0i16; 16], [0i16; 16]);
                let ea = read_coefficients(&mut a, &probs, ctx, first, dq, &mut oa);
                let eb = reference(&mut b, &probs, ctx, first, dq, &mut ob);
                assert_eq!((ea, oa), (eb, ob), "case {case} block {blk}");
            }
            assert_eq!(a.read_literal(16), b.read_literal(16), "case {case}: decoder state afterwards");
        }
    }
}
