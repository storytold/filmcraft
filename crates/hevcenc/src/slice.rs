//! Single-slice HEVC picture encoder (H.265 §7.3.6.1, §7.3.8.5–§7.3.8.11, §8–§9.3).
//!
//! Phase 1, Main 4:2:0 8-bit, CTU 64, one slice per picture, deterministic single-threaded coding (no threads inside
//! slice coding; identical input yields byte-identical output).
//!
//! Pictures: IDR (NAL 20, `slice_type` 2) on frame 0, every `keyint` pictures and lookahead scene cuts; else P
//! (NAL 1 `TRAIL_R`, `slice_type` 1). POC 0,1,2… resets on IDR. `first_slice_segment_in_pic_flag = 1`,
//! `no_output_of_prior_pics_flag = 1` on IRAP, pps id 0, RPS via `short_term_ref_pic_set_sps_flag = 1` idx 0 (our SPS
//! has exactly one set: 1 negative pic, delta 1), no ref override / lists-mod (1 ref), `five_minus_max_num_merge_cand = 0`
//! (maxCand 5), `slice_qp_delta = SliceQpY - init_qp_abs`, always writes `slice_loop_filter_across_slices_flag = 1`
//! (coded because `pps_loop_filter_across_slices = 1` with SAO off and deblocking on, §7.3.6.1), then `byte_alignment`.
//! P slices carry `slice_pic_order_cnt_lsb` in `log2_max_poc_lsb` bits. No SAO (SPS off), no TMVP/collocated (SPS off),
//! no weighted (PPS off), no deblocking override (PPS off), no entry points / extension.
//!
//! CTUs 64 in raster order; CUs split 64→32→16→8 (min 8, and 64 always splits: no 64-leaf coding in phase 1) when
//! children SATD-sum + split-bit beats the parent (`SATD + λ·bits` with [`CostTable`]/[`bin_cost`],
//! `λ = 2^((QP-12)/3)`). Every CU boundary sets BOTH `tu_edge` and `pu_edge` on the 8x8 [`DeblockMaps`] grid
//! (the deblock equivalence assumption).
//!
//! Intra CUs are 2Nx2N only (no NxN/AMP, `part_mode` coded only for 8x8 CUs); luma tries SATD over the candidate set
//! then full RD (`SSD + λ·estimator bits`) over the best 3 including MPM handling (`prev_intra_luma_flag` / `mpm_idx`
//! 2 bypass bins / `rem_intra_luma_pred_mode` 5 bypass bits with sorted-list adjustment, §9.3.3); chroma tries DM + 0..3
//! best by RD and signals `intra_chroma_pred_mode` Table 8-2 aware via [`resolve_chroma_mode`]. P pictures code every
//! CU as inter (no intra in P; scene cuts become IDRs, so this only costs compression, never correctness).
//!
//! Inter PUs are 2Nx2N only, single ref idx 0, tried in RD order: P-skip (zero MV + zero residual, `merge_idx` of the zero
//! candidate), merge over the 5-candidate spatial list (A1/B1/B0/A0/B2 from the 4x4 MV/mode grid), AMVP with [`search`]
//! (±63 luma pels, predictor is the median of left/above/above-right spatial MVs, falling back to the available subset
//! or zero). MVP derivation uses the same-ref fast path (single reference: scaling is identity). `mvp_lX_flag` +
//! MVD via `mvd_coding` (`abs_mvd_greater0/1` + EG1 bypass remainder + sign). A merge winner whose residual trials all
//! zero is re-emitted as skip (merge infers `rqt_root_cbf = 1`, which forbids the all-zero case). Balanced/Speed pick
//! min `SATD + λ·bits`; Quality re-scores the top 2 with `SSD + λ·estimator bits` (RD-lite).
//!
//! Presets (exact differences): Speed = intra SATD shortlist 8 (`{0,1,10,26}` + MPM0 ±1,±2 clamped to 2..34, deduped,
//! filled with 2,18,34) + half-pel refine only (quarter-pel [`search`] result rounded to half-pel) + no RD-lite.
//! Balanced = full 35-mode SATD + RD over best 3 + full quarter-pel refine. Quality = Balanced + RD-lite (top-2 SSD
//! rescore). TUs are always root-only (`split_transform_flag = 0`): TU-split RD is a follow-up.
//!
//! TUs: root `min(CU,32)` (CU ≤ 32 always, since 64 always CU-splits); `cbf_cb/cr` use `CBF_CHROMA + 0`,
//! `cbf_luma` uses `CBF_LUMA + 1` at depth 0 and is SKIPPED for inter roots with both chroma flags zero (decoded as
//! inferred 1, §7.3.8.8), `split_transform_flag` uses `SPLIT_TRANSFORM + (5 - log2Tb)`, all mirroring `slicedec.rs`.
//! Residual coding uses spec scans implemented in-slice (up-right diagonal 6.5.3 + 4x4 sub-block scans 6.5.4/6.5.5):
//! `last_sig_coeff_x/y_prefix` with luma `off = 3*(log2-2)+((log2-1)>>2)` / chroma `off = 15` and shifts,
//! `coded_sub_block_flag` (middle sub-blocks only; DC and last-containing are inferred 1),
//! `sig_coeff_flag` (4x4 `CTX_IDX_MAP`, else `prev_csbf`/position/`xs+ys`/`log2`/`scanIdx` rules, with per-sub-block
//! DC inference exactly like the decoder: inferred iff no significant coeff found yet in the sub-block),
//! `coeff_abs_level_greater1/2`, `coeff_abs_level_remaining` (rice-adaptive). Greater/sign/remaining follow the
//! significant list in REVERSE scan order (last-to-first), like the decoder.
//!
//! QP: frame QP from [`Rc`]; per-CTU delta from [`aq_offsets`] (exactly 0 when strength is 0); desired CTU QP
//! `clamp(slice_qp + round(aq))`, signalled delta `desired - qp_pred` (first CTU equals `round(aq)`); coded once per
//! CTU that has coeffs (`IsCuQpDeltaCoded`, §7.3.8.5/§8.10, QG = CTU since `diff_cu_qp_delta_depth = 0`); deblock
//! `qp_y` per 8x8 is the final block QP (per-TU-accurate: only blocks covered by a nonzero luma TU are marked).
//! Recon: predict → residual → fwd → quant → dequant → inv → add+clip → store; after all CTUs [`deblock_picture`],
//! then DPB holds this picture as the single ref (`RefPic`, `extend_edges`); recon capture is post-deblock when
//! enabled. CABAC `initType` 0 (I/IDR) / 1 (P), `encode_term` after every CTU but the last, `finish` after the last;
//! bytes are escaped at NAL assembly.

use crate::Error;
use crate::Preset;
use crate::cabac::{BinSink, CabacEncoder, CabacEstimator, CostTable, CtxStates, bin_cost};
use crate::deblock::{DeblockMaps, deblock_picture};
use crate::dsp::{satd, ssd};
use crate::inter::{MergeCand, Mv, mc_chroma, mc_luma, merge_list, search};
use crate::intra::{mpm_candidates, predict_chroma, predict_luma, prepare_chroma_refs, prepare_luma_refs, resolve_chroma_mode};
use crate::params::NAL_IDR_N_LP;
use crate::picture::{Frame, RefPic};
use crate::tables::{
    ABS_MVD_GT0, ABS_MVD_GT1, CBF_CHROMA, CBF_LUMA, CODED_SUB_BLOCK, CU_QP_DELTA_ABS, CU_SKIP, GT1, GT2, INTRA_CHROMA, LAST_X_PREFIX, LAST_Y_PREFIX,
    MERGE_FLAG, MERGE_IDX, MVP_FLAG, PART_MODE, PRED_MODE, PREV_INTRA_LUMA, RQT_ROOT_CBF, SIG_COEFF, SPLIT_CU, SPLIT_TRANSFORM,
};
use crate::transform::{Deadzone, QuantParams, dequant_block, fwd_block, inv_block, quant_block};
use filmcraft_bitstream::BitWriter;

/// Luma CTB size.
pub const CTU: usize = 64;
/// Reference padding covering the ±63-pel search window plus the 8-tap/4-tap
/// filter overhang (conservative 72 so every searched MV stays in-bounds).
pub const REF_PAD: usize = 72;
/// Slice NAL type for P pictures (`TRAIL_R`).
pub const NAL_TRAIL_R: u8 = 1;
/// Max merge candidates (`five_minus_max_num_merge_cand = 0`).
pub const MAX_MERGE: usize = 5;
/// Search range in luma pels (±63, i.e. ±252 qpel, inside the VUI ±256 window).
pub const SEARCH_RANGE: u32 = 63;
/// Prefix SEI UUID: ASCII bytes of `FCHEVCENC07200001`.
const SEI_UUID: [u8; 16] = [70, 67, 72, 69, 86, 67, 69, 78, 67, 48, 55, 50, 48, 48, 48, 49];
/// 4x4 `sig_coeff_flag` position map, same values as the decoder `CTX_IDX_MAP` (Table 9-50).
const CTX_IDX_MAP_4X4: [u8; 16] = [0, 1, 4, 5, 2, 3, 4, 5, 6, 6, 8, 8, 7, 7, 8, 8];

/// `λ = 2^((QP-12)/3)` for both SATD- and SSD-domain RD costs.
fn lambda_for_qp(qp: u8) -> f64 {
    2f64.powf((f64::from(qp.min(51)) - 12.0) / 3.0)
}

/// Clamp to the QP range.
fn clip_qp(v: i32) -> u8 {
    v.clamp(0, 51) as u8
}

/// Context index with saturation (mirrors `CtxStates::idx` for hostile inputs).
fn ctx_of(states: &CtxStates, base: usize, inc: usize) -> usize {
    states.idx(base, inc)
}

/// AUD RBSP: 3-bit `primary_pic_type` (0 = I/IDR, 1 = P) plus `rbsp_trailing_bits`.
pub fn aud_rbsp(primary_pic_type: u8) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(u32::from(primary_pic_type & 7), 3);
    w.rbsp_trailing();
    w.finish()
}

/// Prefix `user_data_unregistered` SEI RBSP: `uuid_iso_iec_11578` + `filmcraft-hevcenc {version} Main`.
pub fn sei_prefix_rbsp() -> Vec<u8> {
    let mut payload = SEI_UUID.to_vec();
    payload.extend_from_slice(format!("filmcraft-hevcenc {} Main", env!("CARGO_PKG_VERSION")).as_bytes());
    let mut w = BitWriter::new();
    w.write_bits(5, 8); // payloadType user_data_unregistered
    let mut size = payload.len();
    while size >= 255 {
        w.write_bits(255, 8);
        size -= 255;
    }
    w.write_bits(size as u32, 8);
    w.write_bytes(&payload);
    w.rbsp_trailing();
    w.finish()
}

/// Coded picture output: the complete slice NAL (header included, framing excluded).
pub struct PictureResult {
    pub slice_nal: Vec<u8>,
}

/// Write `n` bypass bits MSB-first.
fn write_bypass_bits<S: BinSink>(sink: &mut S, value: u32, n: u32) {
    for i in (0..n.min(32)).rev() {
        sink.bypass(((value >> i) & 1) != 0);
    }
}

/// Encode EG1 remainder for MVD (`t = abs-2`): prefix ones + 0, then `(prefix+1)`-bit suffix.
fn write_eg1<S: BinSink>(sink: &mut S, t: u32) -> Result<(), Error> {
    let mut p = 0u32;
    while p < 30 {
        let base = (1u32.checked_shl(p + 1).unwrap_or(0)).wrapping_sub(2);
        let next = (1u32.checked_shl(p + 2).unwrap_or(0)).wrapping_sub(2);
        if t >= base && t < next {
            break;
        }
        p += 1;
    }
    if p >= 30 {
        return Err(Error::InvalidConfig("mvd remainder too large".to_string()));
    }
    for _ in 0..p {
        sink.bypass(true);
    }
    sink.bypass(false);
    let base = (1u32.checked_shl(p + 1).unwrap_or(0)).wrapping_sub(2);
    write_bypass_bits(sink, t.wrapping_sub(base), p + 1);
    Ok(())
}

/// Encode `coeff_abs_level_remaining` for `rem = level - base` under rice `rice` (mirrors `coeff_abs_level_remaining`).
fn write_coeff_rem<S: BinSink>(sink: &mut S, rem: i32, rice: u32) -> Result<(), Error> {
    if rem < 0 {
        return Err(Error::InvalidConfig("negative coeff remainder".to_string()));
    }
    let r = rem as u32;
    let thr = 4u32.checked_shl(rice).unwrap_or(u32::MAX);
    if r < thr {
        // Short code: `r >> rice` ones (always < 4 here), a zero, then the low `rice` bits of `r`.
        let prefix = if rice >= 32 { 0 } else { r >> rice };
        for _ in 0..prefix.min(4) {
            sink.bypass(true);
        }
        sink.bypass(false);
        if rice > 0 {
            write_bypass_bits(sink, r, rice);
        }
        return Ok(());
    }
    let mut p = 4u32;
    while p < 32 {
        let base = (((1u32.checked_shl(p - 3).unwrap_or(0)).wrapping_add(2)) << rice.min(24)) as u64;
        let size = 1u64 << (p - 3 + rice.min(24)) as u64;
        if (r as u64) >= base && (r as u64) < base + size {
            break;
        }
        p += 1;
    }
    if p >= 32 {
        return Err(Error::InvalidConfig("coeff remainder too large".to_string()));
    }
    for _ in 0..p {
        sink.bypass(true);
    }
    sink.bypass(false);
    let base = ((1u32.checked_shl(p - 3).unwrap_or(0)).wrapping_add(2)) << rice.min(24);
    write_bypass_bits(sink, r.wrapping_sub(base), p - 3 + rice.min(24));
    Ok(())
}

/// Encode EG0 suffix for `cu_qp_delta_abs - 5` (decoder adds `1<<k` per set bypass bit).
fn write_qp_eg0<S: BinSink>(sink: &mut S, rem: u32) -> Result<(), Error> {
    let mut p = 0u32;
    while p < 30 {
        let base = (1u32.checked_shl(p).unwrap_or(0)).wrapping_sub(1);
        let next = (1u32.checked_shl(p + 1).unwrap_or(0)).wrapping_sub(1);
        if rem >= base && rem < next {
            break;
        }
        p += 1;
    }
    if p >= 30 {
        return Err(Error::InvalidConfig("qp delta remainder too large".to_string()));
    }
    for _ in 0..p {
        sink.bypass(true);
    }
    sink.bypass(false);
    let base = (1u32.checked_shl(p).unwrap_or(0)).wrapping_sub(1);
    write_bypass_bits(sink, rem.wrapping_sub(base), p);
    Ok(())
}

/// Up-right diagonal 4x4 scan (6.5.3): `[(0,0),(0,1),(1,0),(0,2),(1,1),(2,0),…,(3,3)]`.
fn diag4_pos() -> [(u8, u8); 16] {
    let mut out = [(0u8, 0u8); 16];
    let mut i = 0usize;
    let mut x = 0i32;
    let mut y = 0i32;
    while i < 16 {
        while y >= 0 {
            if (0..4).contains(&x) && (0..4).contains(&y) {
                out[i] = (x as u8, y as u8);
                i += 1;
                if i >= 16 {
                    break;
                }
            }
            y -= 1;
            x += 1;
        }
        y = x;
        x = 0;
    }
    out
}

/// 4x4 position scan for `scanIdx`: 0 diagonal, 1 horizontal, 2 vertical.
fn pos_scan_4(scan_idx: usize) -> [(u8, u8); 16] {
    if scan_idx == 1 {
        let mut o = [(0u8, 0u8); 16];
        for i in 0..16 {
            o[i] = ((i % 4) as u8, (i / 4) as u8);
        }
        return o;
    }
    if scan_idx == 2 {
        let mut o = [(0u8, 0u8); 16];
        for i in 0..16 {
            o[i] = ((i / 4) as u8, (i % 4) as u8);
        }
        return o;
    }
    diag4_pos()
}

/// Sub-block scan for `log2Sb x log2Sb` sub-blocks (`sb_w = 1 << log2Sb`): diagonal / horizontal / vertical.
fn subblock_scan(log2sb: u32, scan_idx: usize) -> Vec<(u8, u8)> {
    let w = 1usize.checked_shl(log2sb.min(5)).unwrap_or(1);
    let mut out = Vec::with_capacity(w.saturating_mul(w));
    if scan_idx == 1 {
        for y in 0..w {
            for x in 0..w {
                out.push((x as u8, y as u8));
            }
        }
        return out;
    }
    if scan_idx == 2 {
        // Vertical scan walks columns first.
        for x in 0..w {
            for y in 0..w {
                out.push((x as u8, y as u8));
            }
        }
        return out;
    }
    // Diagonal: increasing sums, alternating direction per anti-diagonal like 6.5.3.
    let mut x = 0i32;
    let mut y = 0i32;
    while out.len() < w.saturating_mul(w) {
        while y >= 0 {
            if (0..w as i32).contains(&x) && (0..w as i32).contains(&y) {
                out.push((x as u8, y as u8));
            }
            y -= 1;
            x += 1;
        }
        y = x;
        x = 0;
    }
    out
}

/// `scanIdx` per §7.4.9.11: intra 4x4 luma / 8x8 luma use the mode, else diagonal.
fn scan_idx(is_intra: bool, log2: u32, c: usize, mode: u32) -> usize {
    if is_intra && (log2 == 2 || (log2 == 3 && c == 0)) {
        if (6..=14).contains(&mode) {
            2
        } else if (22..=30).contains(&mode) {
            1
        } else {
            0
        }
    } else {
        0
    }
}

/// 4x4 grid state for merge/MPM/skip/split contexts and intra availability.
struct Grid {
    w4: usize,
    h4: usize,
    mv: Vec<Option<(Mv, i32)>>,
    intra: Vec<bool>,
    skip: Vec<bool>,
    depth: Vec<u8>,
    mode: Vec<Option<u8>>,
    coded: Vec<bool>,
}

impl Grid {
    fn new(coded_w: usize, coded_h: usize) -> Self {
        let (w4, h4) = (coded_w.div_ceil(4), coded_h.div_ceil(4));
        let n = w4.saturating_mul(h4);
        Grid { w4, h4, mv: vec![None; n], intra: vec![false; n], skip: vec![false; n], depth: vec![0; n], mode: vec![None; n], coded: vec![false; n] }
    }
    fn at_px(&self, x: usize, y: usize) -> Option<usize> {
        if x < self.w4.saturating_mul(4) && y < self.h4.saturating_mul(4) {
            Some(y.saturating_div(4).saturating_mul(self.w4).saturating_add(x.saturating_div(4)))
        } else {
            None
        }
    }
    /// Motion at luma `(x,y)` for merge (None = unavailable: outside or not yet coded).
    fn mv_at(&self, x: i32, y: i32) -> Option<(Mv, i32)> {
        if x < 0 || y < 0 {
            return None;
        }
        let i = self.at_px(x as usize, y as usize)?;
        if *self.coded.get(i)? { *self.mv.get(i)? } else { None }
    }
    /// Luma intra mode at `(x,y)` for MPM (None = unavailable or inter).
    fn mode_at(&self, x: i32, y: i32) -> Option<u8> {
        if x < 0 || y < 0 {
            return None;
        }
        let i = self.at_px(x as usize, y as usize)?;
        if *self.coded.get(i)? && *self.intra.get(i)? { *self.mode.get(i)? } else { None }
    }
    fn fill_cu(&mut self, x0: usize, y0: usize, n: usize, mv: Option<(Mv, i32)>, intra: bool, skip: bool, depth: u8, mode: Option<u8>) {
        let (x1, y1) = (x0.saturating_add(n), y0.saturating_add(n));
        for y in (y0..y1.min(self.h4.saturating_mul(4))).step_by(4) {
            for x in (x0..x1.min(self.w4.saturating_mul(4))).step_by(4) {
                if let Some(i) = self.at_px(x, y) {
                    if let Some(s) = self.mv.get_mut(i) {
                        *s = mv;
                    }
                    if let Some(s) = self.intra.get_mut(i) {
                        *s = intra;
                    }
                    if let Some(s) = self.skip.get_mut(i) {
                        *s = skip;
                    }
                    if let Some(s) = self.depth.get_mut(i) {
                        *s = depth;
                    }
                    if let Some(s) = self.mode.get_mut(i) {
                        *s = if intra { mode } else { None };
                    }
                    if let Some(s) = self.coded.get_mut(i) {
                        *s = true;
                    }
                }
            }
        }
    }
}

/// Split-flag ctxInc from left/above depths (§9.3.4.2.2): +1 per neighbour deeper than `depth`.
fn split_inc(grid: &Grid, x0: usize, y0: usize, depth: u8) -> usize {
    let mut inc = 0usize;
    if x0 > 0
        && let Some(i) = grid.at_px(x0.saturating_sub(1), y0)
        && grid.coded.get(i).copied().unwrap_or(false)
        && grid.depth.get(i).copied().unwrap_or(0) > depth
    {
        inc += 1;
    }
    if y0 > 0
        && let Some(i) = grid.at_px(x0, y0.saturating_sub(1))
        && grid.coded.get(i).copied().unwrap_or(false)
        && grid.depth.get(i).copied().unwrap_or(0) > depth
    {
        inc += 1;
    }
    inc.min(2)
}

/// Skip-flag ctxInc from left/above skip flags.
fn skip_inc(grid: &Grid, x0: usize, y0: usize) -> usize {
    let mut inc = 0usize;
    if x0 > 0
        && let Some(i) = grid.at_px(x0.saturating_sub(1), y0)
        && grid.coded.get(i).copied().unwrap_or(false)
        && grid.skip.get(i).copied().unwrap_or(false)
    {
        inc += 1;
    }
    if y0 > 0
        && let Some(i) = grid.at_px(x0, y0.saturating_sub(1))
        && grid.coded.get(i).copied().unwrap_or(false)
        && grid.skip.get(i).copied().unwrap_or(false)
    {
        inc += 1;
    }
    inc.min(2)
}

/// Gather the 5 spatial merge neighbours `[A1,B1,B0,A0,B2]` (§8.5.3.2.3) from the grid.
fn gather_merge(grid: &Grid, xp: i32, yp: i32, w: i32, h: i32) -> [Option<(Mv, i32)>; 5] {
    [grid.mv_at(xp - 1, yp + h - 1), grid.mv_at(xp + w - 1, yp - 1), grid.mv_at(xp + w, yp - 1), grid.mv_at(xp - 1, yp + h), grid.mv_at(xp - 1, yp - 1)]
}

/// Median-ish AMVP predictor: median of left (A1), above (B1), above-right (B0), else available subset or zero.
fn amvp_predictor(grid: &Grid, xp: i32, yp: i32, w: i32, h: i32) -> Mv {
    let mut vs = Vec::with_capacity(3);
    for m in [grid.mv_at(xp - 1, yp + h - 1), grid.mv_at(xp + w - 1, yp - 1), grid.mv_at(xp + w, yp - 1)].into_iter().flatten() {
        vs.push(m.0);
    }
    if vs.is_empty() {
        return Mv { x: 0, y: 0 };
    }
    vs.sort_by_key(|m| (m.x, m.y));
    if vs.len() == 1 {
        vs[0]
    } else if vs.len() == 2 {
        Mv { x: (vs[0].x + vs[1].x) / 2, y: (vs[0].y + vs[1].y) / 2 }
    } else {
        vs[1] // median of three
    }
}

/// AMVP candidate list mirroring the decoder single-ref path: first available of {A0, A1},
/// then first available of {B0, B1, B2} unless equal to the first; missing entries are zero.
/// Positions compress (an unavailable side leaves no hole), so `mvp_flag` indexes the same
/// candidate on both sides. No scaling (one reference is identity) and no temporal (the SPS
/// disables TMVP), matching what the decoder derives in that configuration.
fn amvp_list(grid: &Grid, xp: i32, yp: i32, w: i32, h: i32) -> [Mv; 2] {
    let z = Mv { x: 0, y: 0 };
    let a = grid.mv_at(xp - 1, yp + h).or_else(|| grid.mv_at(xp - 1, yp + h - 1)).map(|m| m.0);
    let b = [grid.mv_at(xp + w, yp - 1), grid.mv_at(xp + w - 1, yp - 1), grid.mv_at(xp - 1, yp - 1)].into_iter().flatten().map(|m| m.0).next();
    let mut list = [z, z];
    if let Some(a) = a {
        list[0] = a;
        if let Some(b) = b
            && b != a
            && let Some(s) = list.get_mut(1)
        {
            *s = b;
        }
    } else if let Some(b) = b
        && let Some(s) = list.get_mut(0)
    {
        *s = b;
    }
    list
}

/// MPM above-mode with the §8.4.2 CTU-row rule: the block above a CTU-row boundary is
/// unavailable for the candidate list, so it contributes DC (`None`). Mirrors the decoder
/// availability gate; without it the encoder signals REM values against a different list than
/// the decoder derives (silent mode mismatch on the first CU row of every CTU row but the first).
/// `CTU` is the fixed phase-1 CTU size, matching the SPS both sides share.
fn mode_above_at(grid: &Grid, x0: i32, y0: i32) -> Option<u8> {
    if y0 > 0 && y0 & (CTU as i32 - 1) != 0 { grid.mode_at(x0, y0 - 1) } else { None }
}

/// `rem_intra_luma_pred_mode` value whose decode yields `mode` (brute 0..32, deterministic).
fn rem_for_mode(mode: u8, sorted: [u8; 3]) -> u8 {
    for rem in 0..32u8 {
        let mut m = rem as u32;
        for c in sorted {
            if m >= u32::from(c) {
                m += 1;
            }
        }
        if m == u32::from(mode) {
            return rem;
        }
    }
    0
}

/// Sample fetch from a padded plane (None outside the visible window).
fn plane_sample(plane: &crate::picture::Plane, x: usize, y: usize) -> Option<u8> {
    if x >= plane.w || y >= plane.h {
        return None;
    }
    let pad = plane.stride.saturating_sub(plane.w) / 2;
    plane.data.get(y.saturating_add(pad).saturating_mul(plane.stride).saturating_add(x.saturating_add(pad))).copied()
}

/// Copy a dense `w x h` luma block from a padded plane at visible `(x0,y0)` (row stride `w`).
fn copy_block(plane: &crate::picture::Plane, x0: usize, y0: usize, w: usize, h: usize, out: &mut [u8]) -> Result<(), Error> {
    if out.len() < w.saturating_mul(h) {
        return Err(Error::InvalidConfig("block buffer too small".to_string()));
    }
    for y in 0..h {
        for x in 0..w {
            let v = plane_sample(plane, x0.saturating_add(x), y0.saturating_add(y)).unwrap_or(128);
            if let Some(s) = out.get_mut(y.saturating_mul(w).saturating_add(x)) {
                *s = v;
            } else {
                return Err(Error::InvalidConfig("block index out of range".to_string()));
            }
        }
    }
    Ok(())
}

/// Whether the 4x4 luma block at `(x,y)` is already coded (raster order); outside the picture reads as missing.
fn coded_4x4(grid: &Grid, x: usize, y: usize) -> bool {
    grid.at_px(x, y).is_some_and(|k| grid.coded.get(k).copied().unwrap_or(false))
}

/// Build substituted luma refs for the TB at `(x0,y0)` size `n` from `recon`; availability is exactly the
/// grid-coded state (raster order), so the decoder substitutes identically. `mode` gates the §8.4.4.2.3 filter.
fn luma_refs(recon: &Frame, grid: &Grid, x0: usize, y0: usize, n: usize, mode: u8) -> Result<crate::intra::PreparedRefs, Error> {
    let need = n.saturating_mul(2);
    let mut top = vec![0u8; need];
    let mut left = vec![0u8; need];
    let mut at = vec![false; need];
    let mut al = vec![false; need];
    for i in 0..need {
        let x = x0.saturating_add(i);
        if y0 > 0 && x < recon.y.w {
            top[i] = plane_sample(&recon.y, x, y0.saturating_sub(1)).unwrap_or(128);
            at[i] = coded_4x4(grid, x, y0 - 1);
        }
        let y = y0.saturating_add(i);
        if x0 > 0 && y < recon.y.h {
            left[i] = plane_sample(&recon.y, x0.saturating_sub(1), y).unwrap_or(128);
            al[i] = coded_4x4(grid, x0 - 1, y);
        }
    }
    let corner = if x0 > 0 && y0 > 0 { plane_sample(&recon.y, x0 - 1, y0 - 1).unwrap_or(128) } else { 128 };
    let ac = x0 > 0 && y0 > 0 && coded_4x4(grid, x0 - 1, y0 - 1);
    prepare_luma_refs(n, &top, &left, corner, &at, &al, ac, mode, true)
}

/// Chroma refs for the 4:2:0 TB at luma `(x0,y0)` (chroma origin halved, size `n/2`); availability follows the
/// luma grid-coded state at the doubled coordinates, matching decoder raster order.
fn chroma_refs(recon: &Frame, grid: &Grid, x0: usize, y0: usize, n: usize, plane: usize) -> Result<crate::intra::PreparedRefs, Error> {
    let nc = n / 2;
    let (cx0, cy0) = (x0 / 2, y0 / 2);
    let pl = if plane == 1 { &recon.u } else { &recon.v };
    let need = nc.saturating_mul(2);
    let mut top = vec![0u8; need.max(1)];
    let mut left = vec![0u8; need.max(1)];
    let mut at = vec![false; need.max(1)];
    let mut al = vec![false; need.max(1)];
    for i in 0..need {
        let cx = cx0.saturating_add(i);
        if cy0 > 0 && cx < pl.w {
            top[i] = plane_sample(pl, cx, cy0.saturating_sub(1)).unwrap_or(128);
            at[i] = coded_4x4(grid, cx.saturating_mul(2), cy0.saturating_sub(1).saturating_mul(2));
        }
        let cy = cy0.saturating_add(i);
        if cx0 > 0 && cy < pl.h {
            left[i] = plane_sample(pl, cx0.saturating_sub(1), cy).unwrap_or(128);
            al[i] = coded_4x4(grid, cx0.saturating_sub(1).saturating_mul(2), cy.saturating_mul(2));
        }
    }
    let corner = if cx0 > 0 && cy0 > 0 { plane_sample(pl, cx0 - 1, cy0 - 1).unwrap_or(128) } else { 128 };
    let ac = cx0 > 0 && cy0 > 0 && coded_4x4(grid, cx0.saturating_sub(1).saturating_mul(2), cy0.saturating_sub(1).saturating_mul(2));
    prepare_chroma_refs(nc, &top, &left, corner, &at, &al, ac)
}

/// Predict a luma TB into dense `out` (stride `n`).
fn predict_luma_tb(recon: &Frame, grid: &Grid, x0: usize, y0: usize, n: usize, mode: u8, out: &mut [u8]) -> Result<(), Error> {
    let refs = luma_refs(recon, grid, x0, y0, n, mode)?;
    predict_luma(mode, n, &refs, out, n)
}

/// Generic `split_cu_flag` writer.
fn write_split<S: BinSink>(sink: &mut S, states: &mut CtxStates, grid: &Grid, x0: usize, y0: usize, depth: u8, split: bool) {
    sink.decision(states, ctx_of(states, SPLIT_CU, split_inc(grid, x0, y0, depth)), split);
}

/// Generic `cu_skip_flag` writer (P slices only).
fn write_skip<S: BinSink>(sink: &mut S, states: &mut CtxStates, grid: &Grid, x0: usize, y0: usize, skip: bool) {
    sink.decision(states, ctx_of(states, CU_SKIP, skip_inc(grid, x0, y0)), skip);
}

/// Generic `prev_intra_luma_pred_flag` + `mpm_idx` / `rem` writer (§9.3.3, mirrors `parse_intra_modes`).
fn write_luma_mode<S: BinSink>(sink: &mut S, states: &mut CtxStates, mode: u8, left: Option<u8>, above: Option<u8>) -> Result<(), Error> {
    let list = mpm_candidates(left, above);
    if let Some(idx) = list.iter().position(|m| *m == mode) {
        sink.decision(states, ctx_of(states, PREV_INTRA_LUMA, 0), true);
        // `mpm_idx`: truncated unary with bypass bins (0 / 10 / 11).
        if idx == 0 {
            sink.bypass(false);
        } else if idx == 1 {
            sink.bypass(true);
            sink.bypass(false);
        } else {
            sink.bypass(true);
            sink.bypass(true);
        }
    } else {
        sink.decision(states, ctx_of(states, PREV_INTRA_LUMA, 0), false);
        let mut sorted = list;
        sorted.sort_unstable();
        write_bypass_bits(sink, u32::from(rem_for_mode(mode, sorted)), 5);
    }
    Ok(())
}

/// Generic `intra_chroma_pred_mode` writer (0 → DM `4`, else 1 + 2 bypass bits).
fn write_chroma_mode<S: BinSink>(sink: &mut S, states: &mut CtxStates, chroma_mode: u8) {
    if chroma_mode == 4 {
        sink.decision(states, ctx_of(states, INTRA_CHROMA, 0), false);
    } else {
        sink.decision(states, ctx_of(states, INTRA_CHROMA, 0), true);
        write_bypass_bits(sink, u32::from(chroma_mode.min(3)), 2);
    }
}

/// Generic `merge_flag` + `merge_idx` writer (first bin context, rest bypass truncated unary).
fn write_merge<S: BinSink>(sink: &mut S, states: &mut CtxStates, merge: bool, idx: usize, max: usize) {
    sink.decision(states, ctx_of(states, MERGE_FLAG, 0), merge);
    if merge && max > 1 {
        let first = idx > 0;
        sink.decision(states, ctx_of(states, MERGE_IDX, 0), first);
        if first {
            let mut i = 1usize;
            while i < idx && i < max.saturating_sub(1) {
                sink.bypass(true);
                i += 1;
            }
            if i < max.saturating_sub(1) {
                sink.bypass(false);
            }
        }
    }
}

/// Generic `mvd_coding` + `mvp_lX_flag` writer (`greater0/1` context, EG1 + sign bypass).
///
/// Order follows the syntax: `mvd_coding` (both `greater0` flags, then `greater1` flags, then the
/// per-component remainder + sign) comes BEFORE `mvp_lX_flag`. Emitting the predictor flag first
/// hands the decoder's `greater0` loop predictor bits as motion bits (silent whole-slice desync on
/// any AMVP block with a nonzero horizontal MVD history).
fn write_amvp<S: BinSink>(sink: &mut S, states: &mut CtxStates, mvp_flag: u32, mvd: [i32; 2]) -> Result<(), Error> {
    let a = [mvd[0].unsigned_abs(), mvd[1].unsigned_abs()];
    let g0 = [a[0] > 0, a[1] > 0];
    sink.decision(states, ctx_of(states, ABS_MVD_GT0, 0), g0[0]);
    sink.decision(states, ctx_of(states, ABS_MVD_GT0, 0), g0[1]);
    let mut g1 = [false, false];
    for k in 0..2 {
        if g0[k] {
            g1[k] = a[k] > 1;
            sink.decision(states, ctx_of(states, ABS_MVD_GT1, 0), g1[k]);
        }
    }
    for k in 0..2 {
        if g0[k] {
            if g1[k] {
                write_eg1(sink, a[k].wrapping_sub(2))?;
            }
            sink.bypass(mvd[k] < 0);
        }
    }
    sink.decision(states, ctx_of(states, MVP_FLAG, 0), mvp_flag != 0);
    Ok(())
}

/// Generic `cu_qp_delta_abs/sign` writer (TU(5) prefix ctx 0,1,1,1,1 + EG0 + sign).
fn write_qp_delta<S: BinSink>(sink: &mut S, states: &mut CtxStates, delta: i32) -> Result<(), Error> {
    let a = delta.unsigned_abs().min(1024);
    let mut v = 0u32;
    while v < 5 && v < a {
        sink.decision(states, ctx_of(states, CU_QP_DELTA_ABS, if v == 0 { 0 } else { 1 }), true);
        v += 1;
    }
    if v < 5 {
        sink.decision(states, ctx_of(states, CU_QP_DELTA_ABS, if v == 0 { 0 } else { 1 }), false);
    } else if a >= 5 {
        write_qp_eg0(sink, a.wrapping_sub(5))?;
    }
    if a != 0 {
        sink.bypass(delta < 0);
    }
    Ok(())
}

/// Prefix index for one `last_sig_coeff` coordinate: the value whose suffix range holds `last`
/// (truncated unary, `cmax` bins max).
fn last_prefix(last: u32, cmax: u32) -> u32 {
    if last <= 3 {
        last.min(cmax)
    } else {
        let mut found = cmax;
        for cand in 4..=cmax.max(4) {
            let nb = (cand >> 1).saturating_sub(1);
            let base_v = (1u32.checked_shl(nb).unwrap_or(0)).saturating_mul(2 + (cand & 1));
            let next_nb = ((cand + 1) >> 1).saturating_sub(1);
            let next_v = if cand >= cmax { u32::MAX } else { (1u32.checked_shl(next_nb).unwrap_or(0)).saturating_mul(2 + ((cand + 1) & 1)) };
            if last >= base_v && last < next_v {
                found = cand;
                break;
            }
        }
        found
    }
}

/// `last_sig_coeff_x/y_prefix` + suffix writer (mirrors the decoder `off/shift/cmax` derivation).
///
/// `residual_coding` orders both prefixes before either suffix: emitting a suffix right after its
/// own prefix would hand the decoder's `last_sig_coeff_y_prefix` loop bypass bits as context bins
/// (silent whole-slice desync as soon as either coordinate exceeds 3).
fn write_last_pos<S: BinSink>(sink: &mut S, states: &mut CtxStates, log2: u32, c: usize, last_x: u32, last_y: u32) -> Result<(), Error> {
    let cmax = log2.checked_shl(1).unwrap_or(0).saturating_sub(1);
    let (off, shift) = if c == 0 {
        (3u32.saturating_mul(log2.saturating_sub(2)).saturating_add((log2.saturating_sub(1)) >> 2), (log2 + 1) >> 2)
    } else {
        (15, log2.saturating_sub(2))
    };
    let comps = [(LAST_X_PREFIX, last_x), (LAST_Y_PREFIX, last_y)];
    let mut pxs = [0u32; 2];
    for (k, (base, last)) in comps.iter().enumerate() {
        let px = last_prefix(*last, cmax);
        if let Some(s) = pxs.get_mut(k) {
            *s = px;
        }
        let mut p = 0u32;
        while p < px.min(cmax) {
            sink.decision(states, ctx_of(states, *base, (off + (p >> shift)) as usize), true);
            p += 1;
        }
        if px < cmax {
            sink.decision(states, ctx_of(states, *base, (off + (px >> shift)) as usize), false);
        }
    }
    for ((_, last), px) in comps.iter().zip(pxs.iter()) {
        if *px > 3 {
            let nb = (*px >> 1).saturating_sub(1);
            let base_v = (1u32.checked_shl(nb).unwrap_or(0)).saturating_mul(2 + (*px & 1));
            write_bypass_bits(sink, last.wrapping_sub(base_v), nb);
        }
    }
    Ok(())
}

/// Full `residual_coding` coefficient writer for one TB (SIG/GT1/GT2/remaining + signs, REVERSE scan order).
fn write_coeffs<S: BinSink>(sink: &mut S, states: &mut CtxStates, levels: &[i32], log2: u32, c: usize, intra_mode: u32, is_intra: bool) -> Result<(), Error> {
    let n = 1usize.checked_shl(log2).unwrap_or(4);
    if levels.len() < n.saturating_mul(n) {
        return Err(Error::InvalidConfig("levels too short".to_string()));
    }
    // Locate the last significant coefficient in scan order.
    let sidx = scan_idx(is_intra, log2, c, intra_mode);
    let log2sb = log2.saturating_sub(2);
    let sb_scan = subblock_scan(log2sb, sidx);
    let pos_scan = pos_scan_4(sidx);
    let mut last = (0u32, 0u32);
    'find: for (sx, sy) in sb_scan.iter().rev() {
        for (px, py) in pos_scan.iter().rev() {
            let (x, y) = (*sx as usize * 4 + *px as usize, *sy as usize * 4 + *py as usize);
            if x < n && y < n && *levels.get(y.saturating_mul(n).saturating_add(x)).unwrap_or(&0) != 0 {
                last = (x as u32, y as u32);
                break 'find;
            }
        }
    }
    write_last_pos(sink, states, log2, c, if sidx == 2 { last.1 } else { last.0 }, if sidx == 2 { last.0 } else { last.1 })?;
    let sb_w = 1usize.checked_shl(log2sb).unwrap_or(1);
    let last_sb = sb_scan.iter().position(|s| (s.0 as u32) == last.0 >> 2 && (s.1 as u32) == last.1 >> 2).unwrap_or(0);
    let last_pos = pos_scan.iter().position(|p| (p.0 as u32) == last.0 & 3 && (p.1 as u32) == last.1 & 3).unwrap_or(0);
    let mut csbf = [0u8; 64];
    let mut prev_g1: Option<u32> = None;
    let c_off_sig = if c == 0 { 0 } else { 27 };
    for i in (0..=last_sb).rev() {
        let (xs, ys) = sb_scan.get(i).copied().unwrap_or((0, 0));
        let (xs, ys) = (xs as usize, ys as usize);
        let right = if xs + 1 < sb_w { *csbf.get(ys.saturating_mul(8).saturating_add(xs + 1)).unwrap_or(&0) } else { 0 };
        let below = if ys + 1 < sb_w { *csbf.get(ys.saturating_add(1).saturating_mul(8).saturating_add(xs)).unwrap_or(&0) } else { 0 };
        if i < last_sb && i > 0 {
            let inc = (right | below).min(1) as usize + if c > 0 { 2 } else { 0 };
            // Encoder decides csbf from actual significance in this sub-block.
            let mut any = 0u8;
            for (px, py) in pos_scan {
                let (x, y) = (xs * 4 + px as usize, ys * 4 + py as usize);
                if x < n && y < n && *levels.get(y * n + x).unwrap_or(&0) != 0 {
                    any = 1;
                    break;
                }
            }
            sink.decision(states, ctx_of(states, CODED_SUB_BLOCK, inc), any != 0);
            if let Some(s) = csbf.get_mut(ys * 8 + xs) {
                *s = any;
            }
            if any == 0 {
                continue;
            }
        } else if let Some(s) = csbf.get_mut(ys * 8 + xs) {
            *s = 1;
        }
        // Significance map in reverse scan order; per-sub-block DC inference exactly like the decoder
        // (inferred iff no significant coeff found yet in this sub-block).
        let start: i32 = if i == last_sb { last_pos as i32 - 1 } else { 15 };
        let mut infer_dc = i < last_sb && i > 0;
        let mut sig_idx = Vec::new();
        if i == last_sb {
            sig_idx.push(last_pos);
        }
        if *csbf.get(ys * 8 + xs).unwrap_or(&0) != 0 {
            let prev_csbf = right + (below << 1);
            let mut np = start;
            while np >= 0 {
                let (xp, yp) = pos_scan[np as usize];
                let (xc, yc) = (xs * 4 + xp as usize, ys * 4 + yp as usize);
                let is_last = i == last_sb && np as usize == last_pos;
                if !is_last && xc < n && yc < n {
                    if np > 0 || !infer_dc {
                        let sig_ctx = if log2 == 2 {
                            usize::from(*CTX_IDX_MAP_4X4.get(yc.saturating_mul(4).saturating_add(xc)).unwrap_or(&0))
                        } else if xc + yc == 0 {
                            0
                        } else {
                            let mut s = match prev_csbf {
                                0 => {
                                    if xp as usize + yp as usize == 0 {
                                        2
                                    } else if (xp as usize) + (yp as usize) < 3 {
                                        1
                                    } else {
                                        0
                                    }
                                }
                                1 => {
                                    if yp == 0 {
                                        2
                                    } else if yp == 1 {
                                        1
                                    } else {
                                        0
                                    }
                                }
                                2 => {
                                    if xp == 0 {
                                        2
                                    } else if xp == 1 {
                                        1
                                    } else {
                                        0
                                    }
                                }
                                _ => 2,
                            };
                            if c == 0 {
                                if xs + ys > 0 {
                                    s += 3;
                                }
                                s += if log2 == 3 { if sidx == 0 { 9 } else { 15 } } else { 21 };
                            } else {
                                s += if log2 == 3 { 9 } else { 12 };
                            }
                            s as usize
                        };
                        let v = *levels.get(yc * n + xc).unwrap_or(&0) != 0;
                        sink.decision(states, ctx_of(states, SIG_COEFF, c_off_sig + sig_ctx), v);
                        if v {
                            sig_idx.push(np as usize);
                            infer_dc = false;
                        }
                    } else {
                        sig_idx.push(0);
                    }
                }
                np -= 1;
            }
        }
        if sig_idx.is_empty() {
            continue;
        }
        // GT1/GT2/signs/remaining over the significant list in REVERSE scan order (decoder order).
        let order: Vec<usize> = sig_idx;
        let mut ctx_set = if i == 0 || c > 0 { 0 } else { 2 };
        if let Some(g) = prev_g1
            && g == 0
        {
            ctx_set += 1;
        }
        let mut g1ctx = 1u32;
        let mut gt1 = vec![0u8; order.len()];
        let mut first_g1: Option<usize> = None;
        let c_off_g1 = if c > 0 { 16 } else { 0 };
        for (k, np) in order.iter().enumerate().take(8.min(order.len())) {
            let (x, y) = (xs * 4 + pos_scan[*np].0 as usize, ys * 4 + pos_scan[*np].1 as usize);
            let a = levels.get(y * n + x).unwrap_or(&0).unsigned_abs();
            let f = a > 1;
            sink.decision(states, ctx_of(states, GT1, ctx_set * 4 + (g1ctx.min(3) as usize) + c_off_g1), f);
            gt1[k] = u8::from(f);
            if f {
                g1ctx = 0;
                if first_g1.is_none() {
                    first_g1 = Some(k);
                }
            } else if g1ctx > 0 {
                g1ctx += 1;
            }
        }
        prev_g1 = Some(g1ctx);
        let mut gt2 = 0u8;
        if let Some(k) = first_g1 {
            let (x, y) = (xs * 4 + pos_scan[order[k]].0 as usize, ys * 4 + pos_scan[order[k]].1 as usize);
            let a = levels.get(y * n + x).unwrap_or(&0).unsigned_abs();
            let f = a > 2;
            sink.decision(states, ctx_of(states, GT2, ctx_set + if c > 0 { 4 } else { 0 }), f);
            gt2 = u8::from(f);
        }
        // Signs: one bypass bin per coeff in the same (reverse) order.
        for np in &order {
            let (x, y) = (xs * 4 + pos_scan[*np].0 as usize, ys * 4 + pos_scan[*np].1 as usize);
            let v = *levels.get(y * n + x).unwrap_or(&0);
            if v != 0 {
                sink.bypass(v < 0);
            }
        }
        // Remaining levels with adaptive rice, same order.
        let mut rice = 0u32;
        for (k, np) in order.iter().enumerate() {
            let (x, y) = (xs * 4 + pos_scan[*np].0 as usize, ys * 4 + pos_scan[*np].1 as usize);
            let a = levels.get(y * n + x).unwrap_or(&0).unsigned_abs();
            let mut base = 1i32 + i32::from(*gt1.get(k).unwrap_or(&0));
            if Some(k) == first_g1 {
                base += i32::from(gt2);
            }
            let thresh = if k < 8 { if Some(k) == first_g1 { 3 } else { 2 } } else { 1 };
            if base == thresh {
                write_coeff_rem(sink, a as i32 - base, rice)?;
                if a > 3 * (1 << rice.min(10)) {
                    rice = (rice + 1).min(4);
                }
            }
        }
    }
    Ok(())
}

/// Forward + quant trial for one TB (no CABAC): returns levels, non-zero count and SSD vs `orig`.
fn trial_tb(orig: &[u8], pred: &[u8], n: usize, dst: bool, qp: u8, dz: Deadzone) -> (Vec<i32>, u32, u64) {
    let mut res = vec![0i16; n.saturating_mul(n)];
    for i in 0..n.saturating_mul(n) {
        let o = *orig.get(i).unwrap_or(&128);
        let p = *pred.get(i).unwrap_or(&128);
        res[i] = (i32::from(o) - i32::from(p)) as i16;
    }
    let mut coeff = vec![0i32; n.saturating_mul(n)];
    fwd_block(&res, n, n, dst, &mut coeff);
    let nz = quant_block(&mut coeff, n, qp, dz);
    // SSD of the quantised-then-reconstructed block needs dequant+inv; compute here for RD.
    let mut dq = coeff.clone();
    dequant_block(&mut dq, n, qp);
    let mut rec = vec![0i32; n.saturating_mul(n)];
    inv_block(&dq, n, dst, &mut rec);
    let mut s: u64 = 0;
    for i in 0..n.saturating_mul(n) {
        let r = (i32::from(*pred.get(i).unwrap_or(&128)) + *rec.get(i).unwrap_or(&0)).clamp(0, 255) as u8;
        let d = i32::from(*orig.get(i).unwrap_or(&128)) - i32::from(r);
        s += (d * d) as u64;
    }
    (coeff, nz, s)
}

/// Reconstruct one TB from `levels` into `recon` at `(x0,y0)` (luma coords for c 0, chroma coords else).
fn recon_tb(recon: &mut Frame, x0: usize, y0: usize, n: usize, c: usize, levels: &[i32], qp: u8, dst: bool) -> Result<(), Error> {
    if levels.iter().all(|v| *v == 0) {
        return Ok(());
    }
    let mut dq = levels.to_vec();
    dequant_block(&mut dq, n, qp);
    let mut rec = vec![0i32; n.saturating_mul(n)];
    inv_block(&dq, n, dst, &mut rec);
    let (plane, px0, py0) = if c == 0 {
        (&mut recon.y, x0, y0)
    } else if c == 1 {
        (&mut recon.u, x0 / 2, y0 / 2)
    } else {
        (&mut recon.v, x0 / 2, y0 / 2)
    };
    // NOTE: callers pass luma coords for c 0 and already-halved coords are NOT re-halved here;
    // chroma callers below pass the halved origin directly (see the single call site note).
    for y in 0..n {
        for x in 0..n {
            let r = *rec.get(y.saturating_mul(n).saturating_add(x)).unwrap_or(&0);
            let o = plane_sample(plane, px0.saturating_add(x), py0.saturating_add(y)).unwrap_or(128);
            let v = (i32::from(o) + r).clamp(0, 255) as u8;
            let pad = plane.stride.saturating_sub(plane.w) / 2;
            if px0.saturating_add(x) < plane.w
                && py0.saturating_add(y) < plane.h
                && let Some(s) = plane
                    .data
                    .get_mut(py0.saturating_add(y).saturating_add(pad).saturating_mul(plane.stride).saturating_add(px0.saturating_add(x).saturating_add(pad)))
            {
                *s = v;
            }
        }
    }
    Ok(())
}

/// Per-picture coding context (CABAC, maps, grids, QP).
struct PicCtx {
    states: CtxStates,
    enc: CabacEncoder,
    table: CostTable,
    grid: Grid,
    maps_w: usize,
    maps_h: usize,
    intra_map: Vec<bool>,
    cbf_map: Vec<bool>,
    mv_map: Vec<(i32, i32, i32)>,
    tu_v: Vec<bool>,
    tu_h: Vec<bool>,
    pu_v: Vec<bool>,
    pu_h: Vec<bool>,
    qp_map: Vec<u8>,
    last_qp: i32,
    is_qp_coded: bool,
    qp_pred: i32,
    ctu_qp: u8,
    preset: Preset,
    lambda: f64,
}

impl PicCtx {
    /// QpY the decoder derives for the just-coded CU: predicted QP before the
    /// first `cu_qp_delta` in this CTU (and for all-zero CTUs), coded CTU QP after.
    fn cu_qp(&self) -> u8 {
        if self.is_qp_coded { self.ctu_qp } else { clip_qp(self.qp_pred) }
    }
    /// Mark CU boundaries BOTH tu and pu (mandatory equivalence assumption).
    fn mark_cu(&mut self, x0: usize, y0: usize, n: usize) {
        let (w8, h8) = (self.maps_w, self.maps_h);
        let (bx0, by0) = (x0 / 8, y0 / 8);
        let (bx1, by1) = ((x0.saturating_add(n)).div_ceil(8).min(w8), (y0.saturating_add(n)).div_ceil(8).min(h8));
        for by in by0..by1 {
            if let Some(s) = self.tu_v.get_mut(by.saturating_mul(w8).saturating_add(bx0.min(w8.saturating_sub(1)))) {
                *s = true;
            }
            if let Some(s) = self.pu_v.get_mut(by.saturating_mul(w8).saturating_add(bx0.min(w8.saturating_sub(1)))) {
                *s = true;
            }
        }
        for bx in bx0..bx1 {
            if let Some(s) = self.tu_h.get_mut(by0.min(h8.saturating_sub(1)).saturating_mul(w8).saturating_add(bx)) {
                *s = true;
            }
            if let Some(s) = self.pu_h.get_mut(by0.min(h8.saturating_sub(1)).saturating_mul(w8).saturating_add(bx)) {
                *s = true;
            }
        }
    }
    /// CABAC decision with hoisted context index (avoids double-borrow of `states`).
    fn dec(&mut self, base: usize, inc: usize, bin: bool) {
        let c = self.states.idx(base, inc);
        self.enc.decision(&mut self.states, c, bin);
    }
}

/// Mark the 8x8 blocks covered by an `n x n` luma TU at `(x0,y0)` as holding coefficients.
fn mark_luma_cbf(ctx: &mut PicCtx, x0: usize, y0: usize, n: usize) {
    for by in (y0 / 8)..((y0 + n).div_ceil(8).min(ctx.maps_h)) {
        for bx in (x0 / 8)..((x0 + n).div_ceil(8).min(ctx.maps_w)) {
            if let Some(s) = ctx.cbf_map.get_mut(by * ctx.maps_w + bx) {
                *s = true;
            }
        }
    }
}

/// Speed shortlist (8 modes): `{0,1,10,26}` + MPM0 ±1,±2 clamped to 2..34, deduped, filled with 2,18,34.
fn speed_shortlist(mpm0: u8) -> [u8; 8] {
    let mut tmp = [0u8; 8];
    let mut seen = [false; 35];
    let mut c = 0usize;
    let mut push = |m: u8| {
        if m <= 34 && !seen[m as usize] && c < 8 {
            seen[m as usize] = true;
            tmp[c] = m;
            c += 1;
        }
    };
    for m in [0u8, 1, 10, 26] {
        push(m);
    }
    for d in [-2i16, -1, 0, 1, 2] {
        push((i16::from(mpm0) + d).clamp(2, 34) as u8);
    }
    for m in [2u8, 18, 34] {
        push(m);
    }
    tmp
}

/// Choose the luma intra mode: SATD shortlist then SSD + estimator-bit RD over the best 3.
fn choose_luma_mode(
    src: &Frame,
    recon: &Frame,
    ctx: &PicCtx,
    grid: &Grid,
    x0: usize,
    y0: usize,
    n: usize,
    left: Option<u8>,
    above: Option<u8>,
) -> Result<u8, Error> {
    let mut orig = vec![0u8; n.saturating_mul(n)];
    copy_block(&src.y, x0, y0, n, n, &mut orig)?;
    let cands: Vec<u8> = if ctx.preset == Preset::Speed { speed_shortlist(mpm_candidates(left, above)[0]).to_vec() } else { (0..35u8).collect() };
    // SATD pass.
    let mut scored = Vec::with_capacity(cands.len());
    for m in cands {
        let mut pred = vec![0u8; n.saturating_mul(n)];
        // Unavailable refs fall back to DC (mode 1) for the SATD pass only.
        let ok = predict_luma_tb(recon, grid, x0, y0, n, m, &mut pred).is_ok();
        let p = if ok { pred } else { vec![128u8; n.saturating_mul(n)] };
        let cost = satd(&orig, n, &p, n, n, n);
        scored.push((cost, m, p));
    }
    scored.sort_by_key(|s| s.0);
    scored.truncate(3);
    // Full RD over the best 3 (SSD + λ·estimator bits incl. MPM signalling).
    let mut best = scored.first().map(|s| s.1).unwrap_or(1);
    let mut best_cost = f64::INFINITY;
    for (_, m, p) in scored {
        let dist = ssd(&orig, n, &p, n, n, n) as f64;
        let mut st = ctx.states.clone();
        let mut est = CabacEstimator::new(&ctx.table);
        let _ = write_luma_mode(&mut est, &mut st, m, left, above);
        let bits = est.bits() as f64;
        let cost = dist + ctx.lambda * bits;
        if cost < best_cost {
            best_cost = cost;
            best = m;
        }
    }
    Ok(best)
}

/// Write one intra CU (2Nx2N, single PU, root-only transform tree).
fn code_intra_cu(
    src: &Frame,
    recon: &mut Frame,
    ctx: &mut PicCtx,
    x0: usize,
    y0: usize,
    log2: u32,
    depth: u8,
    luma_mode: u8,
    chroma_mode: u8,
    qp_y: u8,
) -> Result<(u64, bool), Error> {
    if !(3..=5).contains(&log2) {
        return Err(Error::InvalidConfig(format!("intra CU log2 {log2} outside 3..=5")));
    }
    let n = 1usize.checked_shl(log2).unwrap_or(8);
    // part_mode is coded only for 8x8 intra CUs (2Nx2N vs NxN); larger CUs infer 2Nx2N.
    if log2 == 3 {
        ctx.dec(PART_MODE, 0, true);
    }
    let left = if x0 > 0 { ctx.grid.mode_at(x0 as i32 - 1, y0 as i32) } else { None };
    let above = mode_above_at(&ctx.grid, x0 as i32, y0 as i32);
    write_luma_mode(&mut ctx.enc, &mut ctx.states, luma_mode, left, above)?;
    write_chroma_mode(&mut ctx.enc, &mut ctx.states, chroma_mode);
    let eff_c = resolve_chroma_mode(chroma_mode, luma_mode)?;
    // Root-only transform tree: a single TU of min(CU,32) == CU here (CUs are 8..32).
    ctx.dec(SPLIT_TRANSFORM, 5 - log2 as usize, false);
    // Luma TB trial.
    let mut pred = vec![0u8; n.saturating_mul(n)];
    predict_luma_tb(recon, &ctx.grid, x0, y0, n, luma_mode, &mut pred)?;
    let mut orig = vec![0u8; n.saturating_mul(n)];
    copy_block(&src.y, x0, y0, n, n, &mut orig)?;
    let qp = QuantParams::new(qp_y, 0, 0);
    let dst = n == 4;
    let (levels, nz, _) = trial_tb(&orig, &pred, n, dst, qp.qp_y, Deadzone::INTRA);
    // Chroma TBs (4:2:0 halves, 4x4 minimum).
    let cn = (n / 2).max(4);
    let mut cpred_u = vec![0u8; cn.saturating_mul(cn)];
    let mut cpred_v = vec![0u8; cn.saturating_mul(cn)];
    let mut corig_u = vec![0u8; cn.saturating_mul(cn)];
    let mut corig_v = vec![0u8; cn.saturating_mul(cn)];
    {
        let refs_u = chroma_refs(recon, &ctx.grid, x0, y0, n, 1)?;
        let refs_v = chroma_refs(recon, &ctx.grid, x0, y0, n, 2)?;
        let mut tu = vec![0u8; cn.saturating_mul(cn)];
        let mut tv = vec![0u8; cn.saturating_mul(cn)];
        predict_chroma(chroma_mode, luma_mode, cn, &refs_u, &mut tu, cn)?;
        predict_chroma(chroma_mode, luma_mode, cn, &refs_v, &mut tv, cn)?;
        cpred_u = tu;
        cpred_v = tv;
        let _ = eff_c;
    }
    copy_block(&src.u, x0 / 2, y0 / 2, cn, cn, &mut corig_u)?;
    copy_block(&src.v, x0 / 2, y0 / 2, cn, cn, &mut corig_v)?;
    let (levels_u, nz_u, _) = trial_tb(&corig_u, &cpred_u, cn, false, qp.qp_cb, Deadzone::INTRA);
    let (levels_v, nz_v, _) = trial_tb(&corig_v, &cpred_v, cn, false, qp.qp_cr, Deadzone::INTRA);
    // CBF flags (chroma first, then luma).
    ctx.dec(CBF_CHROMA, 0, nz_u > 0);
    ctx.dec(CBF_CHROMA, 0, nz_v > 0);
    ctx.dec(CBF_LUMA, 1, nz > 0);
    let any = (nz + nz_u + nz_v) > 0;
    // The deblock map tracks LUMA occupancy only (chroma edges filter solely at bS 2,
    // which needs an intra side): a chroma-only CU must not raise luma boundary strengths.
    if nz > 0 {
        mark_luma_cbf(ctx, x0, y0, n);
    }
    if any && !ctx.is_qp_coded {
        let delta = i32::from(ctx.ctu_qp) - ctx.qp_pred;
        write_qp_delta(&mut ctx.enc, &mut ctx.states, delta.clamp(-26, 25))?;
        ctx.is_qp_coded = true;
        ctx.last_qp = i32::from(ctx.ctu_qp);
    }
    // Luma residual + recon (pred already staged below before adding).
    for y in 0..n {
        for x in 0..n {
            recon.y.set(x0 + x, y0 + y, *pred.get(y * n + x).unwrap_or(&128));
        }
    }
    if nz > 0 {
        write_coeffs(&mut ctx.enc, &mut ctx.states, &levels, log2, 0, u32::from(luma_mode), true)?;
        recon_tb(recon, x0, y0, n, 0, &levels, qp.qp_y, dst)?;
    }
    // Chroma residual + recon.
    for y in 0..cn {
        for x in 0..cn {
            recon.u.set(x0 / 2 + x, y0 / 2 + y, *cpred_u.get(y * cn + x).unwrap_or(&128));
            recon.v.set(x0 / 2 + x, y0 / 2 + y, *cpred_v.get(y * cn + x).unwrap_or(&128));
        }
    }
    if nz_u > 0 {
        write_coeffs(&mut ctx.enc, &mut ctx.states, &levels_u, cn.trailing_zeros(), 1, u32::from(eff_c), true)?;
        recon_tb(recon, x0, y0, cn, 1, &levels_u, qp.qp_cb, false)?;
    }
    if nz_v > 0 {
        write_coeffs(&mut ctx.enc, &mut ctx.states, &levels_v, cn.trailing_zeros(), 2, u32::from(eff_c), true)?;
        recon_tb(recon, x0, y0, cn, 2, &levels_v, qp.qp_cr, false)?;
    }
    let mut dist = 0u64;
    {
        let mut r = vec![0u8; n * n];
        copy_block(&recon.y, x0, y0, n, n, &mut r).unwrap_or(());
        dist += ssd(&orig, n, &r, n, n, n);
    }
    let _ = depth;
    // The caller records this in the luma deblock map: report luma occupancy, not combined.
    Ok((dist, nz > 0))
}

/// Inter candidate kind for RD ordering.
#[derive(Clone, Copy)]
enum InterKind {
    Skip { idx: usize },
    Merge { idx: usize, mv: Mv },
    Amvp { mvp_flag: u32, mv: Mv, mvd: [i32; 2] },
}

/// Trial the luma + chroma residual of a merge candidate without writing bins (dry run for the skip rule).
fn trial_merge_residual(
    src: &Frame,
    ref_pic: &RefPic,
    x0: usize,
    y0: usize,
    n: usize,
    mv: Mv,
    qp: QuantParams,
) -> Result<(Vec<i32>, u32, Vec<i32>, u32, Vec<i32>, u32, Vec<u8>, Vec<u8>, Vec<u8>), Error> {
    let cn = n / 2;
    let mut pred = vec![0u8; n * n];
    mc_luma(&ref_pic.frame.y, x0, y0, mv, n, n, &mut pred)?;
    let mut orig = vec![0u8; n * n];
    copy_block(&src.y, x0, y0, n, n, &mut orig)?;
    let (lv, nz, _) = trial_tb(&orig, &pred, n, false, qp.qp_y, Deadzone::INTER);
    let mut cu = vec![0u8; cn * cn];
    let mut cv = vec![0u8; cn * cn];
    {
        let mut scratch = vec![0u8; cn * cn];
        mc_chroma(&ref_pic.frame.u, x0 / 2, y0 / 2, mv, cn, cn, &mut cu, &mut scratch)?;
        mc_chroma(&ref_pic.frame.v, x0 / 2, y0 / 2, mv, cn, cn, &mut scratch, &mut cv)?;
    }
    let mut ou = vec![0u8; cn * cn];
    let mut ov = vec![0u8; cn * cn];
    copy_block(&src.u, x0 / 2, y0 / 2, cn, cn, &mut ou)?;
    copy_block(&src.v, x0 / 2, y0 / 2, cn, cn, &mut ov)?;
    let (lvu, nzu, _) = trial_tb(&ou, &cu, cn, false, qp.qp_cb, Deadzone::INTER);
    let (lvv, nzv, _) = trial_tb(&ov, &cv, cn, false, qp.qp_cr, Deadzone::INTER);
    Ok((lv, nz, lvu, nzu, lvv, nzv, pred, cu, cv))
}

/// Write one inter CU (2Nx2N, ref idx 0) and reconstruct it. Returns `(dist, has_cbf)`.
fn code_inter_cu(src: &Frame, recon: &mut Frame, ctx: &mut PicCtx, ref_pic: &RefPic, x0: usize, y0: usize, log2: u32, qp_y: u8) -> Result<(u64, bool), Error> {
    if !(3..=5).contains(&log2) {
        return Err(Error::InvalidConfig(format!("inter CU log2 {log2} outside 3..=5")));
    }
    let n = 1usize.checked_shl(log2).unwrap_or(8);
    let cn = n / 2;
    // Build candidates in RD order: skip, merge x5, AMVP x2.
    let neigh = gather_merge(&ctx.grid, x0 as i32, y0 as i32, n as i32, n as i32);
    let list = merge_list(neigh, MAX_MERGE);
    let zero_idx = list.iter().position(|c| c.mv.x == 0 && c.mv.y == 0).unwrap_or(0);
    let mut orig = vec![0u8; n * n];
    copy_block(&src.y, x0, y0, n, n, &mut orig)?;
    let mut cands: Vec<(f64, InterKind, Vec<u8>)> = Vec::new();
    for (idx, mc) in list.iter().enumerate() {
        let mut pred = vec![0u8; n * n];
        mc_luma(&ref_pic.frame.y, x0, y0, mc.mv, n, n, &mut pred)?;
        let dist = satd(&orig, n, &pred, n, n, n) as f64;
        // Bits: merge_flag 1 + merge_idx (context + bypass) via one-shot prices.
        let mut bits = f64::from(bin_cost(&ctx.states, ctx_of(&ctx.states, MERGE_FLAG, 0), true)) / 64.0;
        if MAX_MERGE > 1 {
            bits += f64::from(bin_cost(&ctx.states, ctx_of(&ctx.states, MERGE_IDX, 0), idx > 0)) / 64.0;
            if idx > 0 {
                bits += (idx.saturating_sub(1)) as f64; // bypass tail, 1 bit each
                if idx < MAX_MERGE - 1 {
                    bits += 1.0; // terminating zero
                }
            }
        }
        if idx == zero_idx {
            let skip_flag_bits = f64::from(bin_cost(&ctx.states, ctx_of(&ctx.states, CU_SKIP, skip_inc(&ctx.grid, x0, y0)), true)) / 64.0;
            cands.push((dist + ctx.lambda * (skip_flag_bits + bits), InterKind::Skip { idx }, pred.clone()));
        }
        cands.push((dist + ctx.lambda * bits, InterKind::Merge { idx, mv: mc.mv }, pred));
    }
    // AMVP: predictor + search (±63) then two mvp flags.
    let amvp = amvp_list(&ctx.grid, x0 as i32, y0 as i32, n as i32, n as i32);
    let predictor = amvp_predictor(&ctx.grid, x0 as i32, y0 as i32, n as i32, n as i32);
    let found = search(&ref_pic.frame.y, &orig, n, x0, y0, n, n, predictor, SEARCH_RANGE);
    // Speed: half-pel refine only (round quarter-pel result to half-pel).
    let found = if ctx.preset == Preset::Speed { Mv { x: found.x & !1, y: found.y & !1 } } else { found };
    for (flag, mvp) in [(0u32, amvp[0]), (1u32, amvp[1])].into_iter() {
        let mvd = [found.x - mvp.x, found.y - mvp.y];
        let mut pred = vec![0u8; n * n];
        mc_luma(&ref_pic.frame.y, x0, y0, found, n, n, &mut pred)?;
        let dist = satd(&orig, n, &pred, n, n, n) as f64;
        // Bits: merge_flag 0 + mvp flag + MVD magnitude estimate (context + bypass count).
        let mut bits = f64::from(bin_cost(&ctx.states, ctx_of(&ctx.states, MERGE_FLAG, 0), false)) / 64.0;
        bits += f64::from(bin_cost(&ctx.states, ctx_of(&ctx.states, MVP_FLAG, 0), flag != 0)) / 64.0;
        for k in 0..2 {
            let a = mvd[k].unsigned_abs();
            bits += f64::from(bin_cost(&ctx.states, ctx_of(&ctx.states, ABS_MVD_GT0, 0), a > 0)) / 64.0;
            if a > 0 {
                bits += f64::from(bin_cost(&ctx.states, ctx_of(&ctx.states, ABS_MVD_GT1, 0), a > 1)) / 64.0;
                bits += 1.0; // sign
                if a > 1 {
                    bits += 2.0; // EG1 remainder proxy (refined by the estimator in Quality)
                }
            }
        }
        // Same-ref fast path: no scaling term (identity).
        cands.push((dist + ctx.lambda * bits, InterKind::Amvp { mvp_flag: flag, mv: found, mvd }, pred));
    }
    cands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    cands.truncate(2);
    // Quality RD-lite: re-score the top 2 with SSD + estimator bits.
    let (kind, pred) = if ctx.preset == Preset::Quality && cands.len() == 2 {
        let mut best = 0usize;
        let mut best_cost = f64::INFINITY;
        for (k, (_, kd, pr)) in cands.iter().enumerate() {
            let dist = ssd(&orig, n, pr, n, n, n) as f64;
            let mut st = ctx.states.clone();
            let mut est = CabacEstimator::new(&ctx.table);
            match *kd {
                InterKind::Skip { idx } => {
                    {
                        let c = st.idx(CU_SKIP, skip_inc(&ctx.grid, x0, y0));
                        est.decision(&mut st, c, true);
                    }
                    write_merge(&mut est, &mut st, true, idx, MAX_MERGE);
                }
                InterKind::Merge { idx, .. } => {
                    {
                        let c = st.idx(CU_SKIP, skip_inc(&ctx.grid, x0, y0));
                        est.decision(&mut st, c, false);
                    }
                    {
                        let c = st.idx(PRED_MODE, 0);
                        est.decision(&mut st, c, false);
                    }
                    {
                        let c = st.idx(PART_MODE, 0);
                        est.decision(&mut st, c, true);
                    }
                    write_merge(&mut est, &mut st, true, idx, MAX_MERGE);
                }
                InterKind::Amvp { mvp_flag, mvd, .. } => {
                    {
                        let c = st.idx(CU_SKIP, skip_inc(&ctx.grid, x0, y0));
                        est.decision(&mut st, c, false);
                    }
                    {
                        let c = st.idx(PRED_MODE, 0);
                        est.decision(&mut st, c, false);
                    }
                    {
                        let c = st.idx(PART_MODE, 0);
                        est.decision(&mut st, c, true);
                    }
                    {
                        let c = st.idx(MERGE_FLAG, 0);
                        est.decision(&mut st, c, false);
                    }
                    write_amvp(&mut est, &mut st, mvp_flag, mvd).unwrap_or(());
                }
            }
            let cost = dist + ctx.lambda * (est.bits() as f64);
            if cost < best_cost {
                best_cost = cost;
                best = k;
            }
        }
        (cands[best].1, cands[best].2.clone())
    } else {
        (cands.first().map(|c| c.1).unwrap_or(InterKind::Skip { idx: zero_idx }), cands.first().map(|c| c.2.clone()).unwrap_or(vec![128u8; n * n]))
    };
    // Signal.
    // A merge candidate whose residual is all zero must travel as skip (a merge-coded root
    // always carries CBFs, so emitting skip bins after `skip_flag = 0` would desync the decoder).
    // Probe BEFORE `skip_flag` is written; the branch below re-trials for levels when nonzero.
    let kind = match kind {
        InterKind::Merge { idx, mv } => {
            let qp = QuantParams::new(qp_y, 0, 0);
            let zero = matches!(
                trial_merge_residual(src, ref_pic, x0, y0, n, mv, qp),
                Ok((_, mnz, _, mnzu, _, mnzv, _, _, _)) if mnz + mnzu + mnzv == 0
            );
            if zero { InterKind::Skip { idx } } else { InterKind::Merge { idx, mv } }
        }
        k => k,
    };
    let is_skip = matches!(kind, InterKind::Skip { .. });
    write_skip(&mut ctx.enc, &mut ctx.states, &ctx.grid, x0, y0, is_skip);
    if is_skip {
        let idx = if let InterKind::Skip { idx } = kind { idx } else { zero_idx };
        return emit_skip(src, recon, ctx, ref_pic, &list, idx, x0, y0, n, cn, log2, &orig, &pred);
    }
    // Non-skip: pred_mode 0 (inter), part 2Nx2N, then merge/AMVP.
    ctx.dec(PRED_MODE, 0, false);
    ctx.dec(PART_MODE, 0, true);
    let (mv, is_merge) = match kind {
        InterKind::Merge { idx, mv } => {
            // Levels for the merge residual (an all-zero residual was converted to skip above,
            // before `skip_flag` was written, so this path always carries coefficients).
            let qp = QuantParams::new(qp_y, 0, 0);
            let (mlv, mnz, mlvu, mnzu, mlvv, mnzv, mpred, mcu, mcv) = trial_merge_residual(src, ref_pic, x0, y0, n, mv, qp)?;
            write_merge(&mut ctx.enc, &mut ctx.states, true, idx, MAX_MERGE);
            // Root-only TU: split flag 0, then chroma CBFs, then luma CBF (skipped when both chroma are zero).
            ctx.dec(SPLIT_TRANSFORM, 5 - log2 as usize, false);
            ctx.dec(CBF_CHROMA, 0, mnzu > 0);
            ctx.dec(CBF_CHROMA, 0, mnzv > 0);
            if mnzu + mnzv > 0 {
                ctx.dec(CBF_LUMA, 1, mnz > 0);
            }
            let any = (mnz + mnzu + mnzv) > 0;
            // Luma occupancy only (see the intra path): chroma-only residue must not raise luma bS.
            if mnz > 0 {
                mark_luma_cbf(ctx, x0, y0, n);
            }
            if any && !ctx.is_qp_coded {
                write_qp_delta(&mut ctx.enc, &mut ctx.states, (i32::from(ctx.ctu_qp) - ctx.qp_pred).clamp(-26, 25))?;
                ctx.is_qp_coded = true;
                ctx.last_qp = i32::from(ctx.ctu_qp);
            }
            // Stage pred + recon, then add residuals from the stored trials (no re-trial: identical inputs).
            stage_pred(recon, x0, y0, n, cn, &mpred, &mcu, &mcv)?;
            let mut dist = 0u64;
            if mnz > 0 {
                write_coeffs(&mut ctx.enc, &mut ctx.states, &mlv, log2, 0, 0, false)?;
                recon_tb(recon, x0, y0, n, 0, &mlv, qp.qp_y, false)?;
            }
            if mnzu > 0 {
                write_coeffs(&mut ctx.enc, &mut ctx.states, &mlvu, (cn.max(4)).trailing_zeros(), 1, 0, false)?;
                recon_tb(recon, x0, y0, cn.max(4), 1, &mlvu, qp.qp_cb, false)?;
            }
            if mnzv > 0 {
                write_coeffs(&mut ctx.enc, &mut ctx.states, &mlvv, (cn.max(4)).trailing_zeros(), 2, 0, false)?;
                recon_tb(recon, x0, y0, cn.max(4), 2, &mlvv, qp.qp_cr, false)?;
            }
            let mut r = vec![0u8; n * n];
            copy_block(&recon.y, x0, y0, n, n, &mut r).unwrap_or(());
            dist += ssd(&orig, n, &r, n, n, n);
            ctx.grid.fill_cu(x0, y0, n, Some((mv, 0)), false, false, log2_to_depth(log2), None);
            ctx.mark_cu(x0, y0, n);
            let map_qp = ctx.cu_qp();
            mark_maps_inter(ctx, x0, y0, n, mnz > 0, mv, map_qp);
            return Ok((dist, any));
        }
        InterKind::Amvp { mvp_flag, mv, mvd } => {
            write_merge(&mut ctx.enc, &mut ctx.states, false, 0, MAX_MERGE);
            write_amvp(&mut ctx.enc, &mut ctx.states, mvp_flag, mvd)?;
            (mv, false)
        }
        InterKind::Skip { idx } => {
            let mv = list.get(idx).map(|c| c.mv).unwrap_or(Mv { x: 0, y: 0 });
            (mv, true)
        }
    };
    // Chroma prediction for the whole 2Nx2N PU.
    let mut cu = vec![0u8; cn * cn];
    let mut cv = vec![0u8; cn * cn];
    {
        let mut scratch = vec![0u8; cn * cn];
        mc_chroma(&ref_pic.frame.u, x0 / 2, y0 / 2, mv, cn, cn, &mut cu, &mut scratch)?;
        mc_chroma(&ref_pic.frame.v, x0 / 2, y0 / 2, mv, cn, cn, &mut scratch, &mut cv)?;
    }
    // RQT root flag (merge 2Nx2N infers true; AMVP always reaches here as non-merge).
    let qp = QuantParams::new(qp_y, 0, 0);
    if !is_merge {
        // Trial the root luma residual first to know the flag value.
        let (lv, nz, _) = trial_tb(&orig, &pred, n, false, qp.qp_y, Deadzone::INTER);
        let _ = lv;
        ctx.dec(RQT_ROOT_CBF, 0, nz > 0);
        if nz == 0 {
            // No residual at all: recon = pred.
            for y in 0..n {
                for x in 0..n {
                    recon.y.set(x0 + x, y0 + y, *pred.get(y * n + x).unwrap_or(&128));
                }
            }
            for y in 0..cn {
                for x in 0..cn {
                    recon.u.set(x0 / 2 + x, y0 / 2 + y, *cu.get(y * cn + x).unwrap_or(&128));
                    recon.v.set(x0 / 2 + x, y0 / 2 + y, *cv.get(y * cn + x).unwrap_or(&128));
                }
            }
            ctx.grid.fill_cu(x0, y0, n, Some((mv, 0)), false, false, log2_to_depth(log2), None);
            ctx.mark_cu(x0, y0, n);
            let map_qp = ctx.cu_qp();
            mark_maps_inter(ctx, x0, y0, n, false, mv, map_qp);
            return Ok((ssd(&orig, n, &pred, n, n, n), false));
        }
    }
    // Residual path: root-only TU (split flag 0), chroma CBFs, luma CBF, QP delta, coeffs.
    ctx.dec(SPLIT_TRANSFORM, 5 - log2 as usize, false);
    // Copy luma pred into recon first, then add residuals per TU.
    for y in 0..n {
        for x in 0..n {
            recon.y.set(x0 + x, y0 + y, *pred.get(y * n + x).unwrap_or(&128));
        }
    }
    for y in 0..cn {
        for x in 0..cn {
            recon.u.set(x0 / 2 + x, y0 / 2 + y, *cu.get(y * cn + x).unwrap_or(&128));
            recon.v.set(x0 / 2 + x, y0 / 2 + y, *cv.get(y * cn + x).unwrap_or(&128));
        }
    }
    let mut ou = vec![0u8; cn * cn];
    let mut ov = vec![0u8; cn * cn];
    copy_block(&src.u, x0 / 2, y0 / 2, cn, cn, &mut ou)?;
    copy_block(&src.v, x0 / 2, y0 / 2, cn, cn, &mut ov)?;
    let (lvu, nzu, _) = trial_tb(&ou, &cu, cn.max(4), false, qp.qp_cb, Deadzone::INTER);
    let (lvv, nzv, _) = trial_tb(&ov, &cv, cn.max(4), false, qp.qp_cr, Deadzone::INTER);
    ctx.dec(CBF_CHROMA, 0, nzu > 0);
    ctx.dec(CBF_CHROMA, 0, nzv > 0);
    let (lv, nz, _) = trial_tb(&orig, &pred, n, false, qp.qp_y, Deadzone::INTER);
    if nzu + nzv > 0 {
        ctx.dec(CBF_LUMA, 1, nz > 0);
    }
    let any = (nz + nzu + nzv) > 0;
    // Luma occupancy only (see the intra path): chroma-only residue must not raise luma bS.
    if nz > 0 {
        mark_luma_cbf(ctx, x0, y0, n);
    }
    if any && !ctx.is_qp_coded {
        write_qp_delta(&mut ctx.enc, &mut ctx.states, (i32::from(ctx.ctu_qp) - ctx.qp_pred).clamp(-26, 25))?;
        ctx.is_qp_coded = true;
        ctx.last_qp = i32::from(ctx.ctu_qp);
    }
    if nz > 0 {
        write_coeffs(&mut ctx.enc, &mut ctx.states, &lv, log2, 0, 0, false)?;
        recon_tb(recon, x0, y0, n, 0, &lv, qp.qp_y, false)?;
    }
    if nzu > 0 {
        write_coeffs(&mut ctx.enc, &mut ctx.states, &lvu, (cn.max(4)).trailing_zeros(), 1, 0, false)?;
        recon_tb(recon, x0, y0, cn.max(4), 1, &lvu, qp.qp_cb, false)?;
    }
    if nzv > 0 {
        write_coeffs(&mut ctx.enc, &mut ctx.states, &lvv, (cn.max(4)).trailing_zeros(), 2, 0, false)?;
        recon_tb(recon, x0, y0, cn.max(4), 2, &lvv, qp.qp_cr, false)?;
    }
    let mut dist = 0u64;
    {
        let mut r = vec![0u8; n * n];
        copy_block(&recon.y, x0, y0, n, n, &mut r).unwrap_or(());
        dist += ssd(&orig, n, &r, n, n, n);
    }
    ctx.grid.fill_cu(x0, y0, n, Some((mv, 0)), false, false, log2_to_depth(log2), None);
    ctx.mark_cu(x0, y0, n);
    let map_qp = ctx.cu_qp();
    mark_maps_inter(ctx, x0, y0, n, nz > 0, mv, map_qp);
    Ok((dist, any))
}

/// Stage luma + chroma predictions into recon (residuals are added by later `recon_tb` calls).
fn stage_pred(recon: &mut Frame, x0: usize, y0: usize, n: usize, cn: usize, pred: &[u8], cu: &[u8], cv: &[u8]) -> Result<(), Error> {
    for y in 0..n {
        for x in 0..n {
            recon.y.set(x0 + x, y0 + y, *pred.get(y * n + x).unwrap_or(&128));
        }
    }
    for y in 0..cn {
        for x in 0..cn {
            recon.u.set(x0 / 2 + x, y0 / 2 + y, *cu.get(y * cn + x).unwrap_or(&128));
            recon.v.set(x0 / 2 + x, y0 / 2 + y, *cv.get(y * cn + x).unwrap_or(&128));
        }
    }
    Ok(())
}

/// Emit a skip CU (same merge index, zero residual): flags, recon = pred, grid/maps, SSD cost.
fn emit_skip(
    src: &Frame,
    recon: &mut Frame,
    ctx: &mut PicCtx,
    ref_pic: &RefPic,
    list: &[MergeCand],
    idx: usize,
    x0: usize,
    y0: usize,
    n: usize,
    cn: usize,
    log2: u32,
    orig: &[u8],
    pred: &[u8],
) -> Result<(u64, bool), Error> {
    let mv = list.get(idx).map(|c| c.mv).unwrap_or(Mv { x: 0, y: 0 });
    write_merge(&mut ctx.enc, &mut ctx.states, true, idx, MAX_MERGE);
    // Skip has zero residual: recon = pred, no RQT flag, no TU.
    for y in 0..n {
        for x in 0..n {
            recon.y.set(x0 + x, y0 + y, *pred.get(y * n + x).unwrap_or(&128));
        }
    }
    // Chroma MC with the same MV (both planes share one validation each).
    let mut pu = vec![0u8; cn * cn];
    let mut pv = vec![0u8; cn * cn];
    {
        let mut scratch = vec![0u8; cn * cn];
        mc_chroma(&ref_pic.frame.u, x0 / 2, y0 / 2, mv, cn, cn, &mut pu, &mut scratch)?;
        mc_chroma(&ref_pic.frame.v, x0 / 2, y0 / 2, mv, cn, cn, &mut scratch, &mut pv)?;
    }
    for y in 0..cn {
        for x in 0..cn {
            recon.u.set(x0 / 2 + x, y0 / 2 + y, *pu.get(y * cn + x).unwrap_or(&128));
            recon.v.set(x0 / 2 + x, y0 / 2 + y, *pv.get(y * cn + x).unwrap_or(&128));
        }
    }
    let _ = src;
    ctx.grid.fill_cu(x0, y0, n, Some((mv, 0)), false, true, log2_to_depth(log2), None);
    ctx.mark_cu(x0, y0, n);
    let map_qp = ctx.cu_qp();
    mark_maps_inter(ctx, x0, y0, n, false, mv, map_qp);
    Ok((ssd(orig, n, pred, n, n, n), false))
}

fn log2_to_depth(log2: u32) -> u8 {
    (6u32.saturating_sub(log2)).min(3) as u8
}

fn mark_maps_inter(ctx: &mut PicCtx, x0: usize, y0: usize, n: usize, cbf: bool, mv: Mv, qp: u8) {
    let (w8, h8) = (ctx.maps_w, ctx.maps_h);
    for by in (y0 / 8)..((y0 + n).div_ceil(8).min(h8)) {
        for bx in (x0 / 8)..((x0 + n).div_ceil(8).min(w8)) {
            let i = by * w8 + bx;
            if let Some(s) = ctx.intra_map.get_mut(i) {
                *s = false;
            }
            if let Some(s) = ctx.cbf_map.get_mut(i) {
                *s = cbf;
            }
            if let Some(s) = ctx.mv_map.get_mut(i) {
                *s = (mv.x, mv.y, 0);
            }
            if let Some(s) = ctx.qp_map.get_mut(i) {
                *s = qp;
            }
        }
    }
}

fn mark_maps_intra(ctx: &mut PicCtx, x0: usize, y0: usize, n: usize, cbf: bool, qp: u8) {
    let (w8, h8) = (ctx.maps_w, ctx.maps_h);
    for by in (y0 / 8)..((y0 + n).div_ceil(8).min(h8)) {
        for bx in (x0 / 8)..((x0 + n).div_ceil(8).min(w8)) {
            let i = by * w8 + bx;
            if let Some(s) = ctx.intra_map.get_mut(i) {
                *s = true;
            }
            if let Some(s) = ctx.cbf_map.get_mut(i) {
                *s = cbf;
            }
            if let Some(s) = ctx.mv_map.get_mut(i) {
                *s = (0, 0, -1);
            }
            if let Some(s) = ctx.qp_map.get_mut(i) {
                *s = qp;
            }
        }
    }
}

/// Code one CU at `(x0,y0)` `log2`/`depth`: split decision by SATD + split-bit, then intra/inter RD.
/// 64x64 CUs always split (no 64-leaf coding in phase 1); leaves are 8..32.
fn code_cu(
    src: &Frame,
    recon: &mut Frame,
    ctx: &mut PicCtx,
    ref_pic: Option<&RefPic>,
    x0: usize,
    y0: usize,
    log2: u32,
    depth: u8,
    is_idr: bool,
) -> Result<(), Error> {
    if !(3..=6).contains(&log2) {
        return Err(Error::InvalidConfig(format!("CU log2 {log2} outside 3..=6")));
    }
    let n = 1usize.checked_shl(log2).unwrap_or(8);
    let coded_w = recon.y.w;
    let coded_h = recon.y.h;
    if x0 >= coded_w || y0 >= coded_h {
        return Ok(());
    }
    // Split trial (64→32→16→8): children SATD-sum + split-bit vs parent SATD + split-bit.
    // 64x64 always splits: our intra/inter prediction caps at 32x32 TUs and 64x64 PUs are never coded.
    if log2 > 3 {
        let lambda = ctx.lambda;
        let mut porig = vec![0u8; n * n];
        copy_block(&src.y, x0, y0, n, n, &mut porig).unwrap_or(());
        let flat = vec![128u8; n * n];
        let parent_satd = u64::from(satd(&porig, n, &flat, n, n, n));
        let split0 = f64::from(bin_cost(&ctx.states, ctx_of(&ctx.states, SPLIT_CU, split_inc(&ctx.grid, x0, y0, depth)), false)) / 64.0;
        let parent_cost = parent_satd as f64 + lambda * split0;
        let h2 = n / 2;
        let mut sum = 0u64;
        for (dx, dy) in [(0, 0), (h2, 0), (0, h2), (h2, h2)] {
            let mut o = vec![0u8; h2 * h2];
            copy_block(&src.y, x0 + dx, y0 + dy, h2, h2, &mut o).unwrap_or(());
            sum += u64::from(satd(&o, h2, &flat[..h2 * h2], h2, h2, h2));
        }
        let split1 = f64::from(bin_cost(&ctx.states, ctx_of(&ctx.states, SPLIT_CU, split_inc(&ctx.grid, x0, y0, depth)), true)) / 64.0;
        if log2 == 6 || sum as f64 + lambda * split1 < parent_cost {
            write_split(&mut ctx.enc, &mut ctx.states, &ctx.grid, x0, y0, depth, true);
            let hlog = log2 - 1;
            code_cu(src, recon, ctx, ref_pic, x0, y0, hlog, depth + 1, is_idr)?;
            code_cu(src, recon, ctx, ref_pic, x0 + h2, y0, hlog, depth + 1, is_idr)?;
            code_cu(src, recon, ctx, ref_pic, x0, y0 + h2, hlog, depth + 1, is_idr)?;
            code_cu(src, recon, ctx, ref_pic, x0 + h2, y0 + h2, hlog, depth + 1, is_idr)?;
            return Ok(());
        }
        write_split(&mut ctx.enc, &mut ctx.states, &ctx.grid, x0, y0, depth, false);
    }
    // Leaf CU (8..32).
    if is_idr {
        // Intra: choose luma + chroma modes by RD, then code.
        let left = if x0 > 0 { ctx.grid.mode_at(x0 as i32 - 1, y0 as i32) } else { None };
        let above = mode_above_at(&ctx.grid, x0 as i32, y0 as i32);
        let lm = choose_luma_mode(src, recon, ctx, &ctx.grid, x0, y0, n, left, above)?;
        // Chroma: DM + 0..3 best by SSD + estimator bits.
        let cn = (n / 2).max(4);
        let mut cm_best = 4u8;
        let mut best_c = f64::INFINITY;
        for cm in 0..5u8 {
            let refs = chroma_refs(recon, &ctx.grid, x0, y0, n, 1)?;
            let mut tmp = vec![0u8; cn * cn];
            predict_chroma(cm, lm, cn, &refs, &mut tmp, cn)?;
            let mut ou = vec![0u8; cn * cn];
            copy_block(&src.u, x0 / 2, y0 / 2, cn, cn, &mut ou).unwrap_or(());
            let dist = ssd(&ou, cn, &tmp, cn, cn, cn) as f64;
            let mut st = ctx.states.clone();
            let mut est = CabacEstimator::new(&ctx.table);
            write_chroma_mode(&mut est, &mut st, cm);
            let cost = dist + ctx.lambda * (est.bits() as f64);
            if cost < best_c {
                best_c = cost;
                cm_best = cm;
            }
        }
        let (dist, cbf) = code_intra_cu(src, recon, ctx, x0, y0, log2, depth, lm, cm_best, ctx.ctu_qp)?;
        let _ = dist;
        ctx.grid.fill_cu(x0, y0, n, None, true, false, depth, Some(lm));
        ctx.mark_cu(x0, y0, n);
        let map_qp = ctx.cu_qp();
        mark_maps_intra(ctx, x0, y0, n, cbf, map_qp);
    } else {
        // P: skip/merge/AMVP RD.
        let rp = ref_pic.ok_or_else(|| Error::InvalidConfig("P picture without a reference".to_string()))?;
        let (dist, cbf) = code_inter_cu(src, recon, ctx, rp, x0, y0, log2, ctx.ctu_qp)?;
        let _ = (dist, cbf);
    }
    Ok(())
}

/// Slice header bits (§7.3.6.1) up to `byte_alignment` (no SAO/TMVP/weighted/deblocking-override/entry/extension).
fn write_slice_header(w: &mut BitWriter, is_idr: bool, poc: u32, log2_max_poc_lsb: u32, slice_type: u32, qp_delta: i32) {
    w.write_bit(true); // first_slice_segment_in_pic_flag
    if is_idr {
        w.write_bit(true); // no_output_of_prior_pics_flag
    }
    w.write_ue(0); // slice_pic_parameter_set_id
    w.write_ue(slice_type); // 2 = I, 1 = P
    if !is_idr {
        let bits = log2_max_poc_lsb.clamp(4, 16);
        let mask = (1u32.checked_shl(bits).unwrap_or(0)).wrapping_sub(1);
        w.write_bits(poc & mask, bits); // slice_pic_order_cnt_lsb
        w.write_bit(true); // short_term_ref_pic_set_sps_flag (single SPS set, idx 0 inferred)
    }
    if slice_type != 2 {
        w.write_bit(false); // num_ref_idx_active_override_flag (1 ref)
        w.write_ue(0); // five_minus_max_num_merge_cand (maxCand 5)
    }
    w.write_se(qp_delta); // slice_qp_delta = SliceQpY - init_qp_abs
    // pps_loop_filter_across_slices = 1 with SAO off and deblocking on: flag IS coded, always 1.
    w.write_bit(true); // slice_loop_filter_across_slices_enabled_flag
    w.write_bit(true); // alignment_bit_equal_to_one
    // Pad zeros to the byte.
    while !w.is_byte_aligned() {
        w.write_bit(false);
    }
}

/// Code one picture into a complete slice NAL (header included, framing excluded).
pub fn code_picture(
    src: &Frame,
    recon: &mut Frame,
    ref_pic: Option<&RefPic>,
    is_idr: bool,
    poc: u32,
    log2_max_poc_lsb: u32,
    init_qp: i32,
    slice_qp: u8,
    preset: Preset,
    aq: &[f32],
) -> Result<PictureResult, Error> {
    if src.y.w != recon.y.w || src.y.h != recon.y.h {
        return Err(Error::InvalidConfig("src/recon size mismatch".to_string()));
    }
    let (coded_w, coded_h) = (recon.y.w, recon.y.h);
    if coded_w == 0 || coded_h == 0 || !coded_w.is_multiple_of(2) || !coded_h.is_multiple_of(2) {
        return Err(Error::BadFrame);
    }
    let (ctbs_w, ctbs_h) = (coded_w.div_ceil(CTU), coded_h.div_ceil(CTU));
    if ctbs_w == 0 || ctbs_h == 0 {
        return Err(Error::BadFrame);
    }
    // QP delta for the slice header.
    let qp_delta = i32::from(slice_qp) - init_qp;
    let mut hw = BitWriter::new();
    write_slice_header(&mut hw, is_idr, poc, log2_max_poc_lsb, if is_idr { 2 } else { 1 }, qp_delta);
    let header = hw.finish();
    // Picture context.
    let maps_w = coded_w.div_ceil(8);
    let maps_h = coded_h.div_ceil(8);
    let n8 = maps_w.saturating_mul(maps_h);
    let mut ctx = PicCtx {
        states: CtxStates::new(),
        enc: CabacEncoder::new(),
        table: CostTable::new(),
        grid: Grid::new(coded_w, coded_h),
        maps_w,
        maps_h,
        intra_map: vec![is_idr; n8],
        cbf_map: vec![false; n8],
        mv_map: vec![(0, 0, if is_idr { -1 } else { 0 }); n8],
        tu_v: vec![false; n8],
        tu_h: vec![false; n8],
        pu_v: vec![false; n8],
        pu_h: vec![false; n8],
        qp_map: vec![slice_qp; n8],
        last_qp: i32::from(slice_qp),
        is_qp_coded: false,
        qp_pred: i32::from(slice_qp),
        ctu_qp: slice_qp,
        preset,
        lambda: lambda_for_qp(slice_qp),
    };
    ctx.states.init(if is_idr { 0 } else { 1 }, slice_qp);
    // CTU raster loop (IsCuQpDeltaCoded resets per CTU since QG = CTU).
    let total = ctbs_w.saturating_mul(ctbs_h);
    for idx in 0..total {
        let (cx, cy) = (idx % ctbs_w, idx / ctbs_w);
        let (x0, y0) = (cx * CTU, cy * CTU);
        // Per-CTU QP: desired = clamp(slice_qp + round(aq)), delta = desired - pred.
        let aqv = aq.get(idx).copied().unwrap_or(0.0);
        let desired = clip_qp(i32::from(slice_qp) + aqv.round() as i32);
        // qp_pred for the CTU start is last_qp (decoder derive_qp_pred with QG = CTU).
        ctx.qp_pred = ctx.last_qp;
        ctx.ctu_qp = desired;
        ctx.is_qp_coded = false;
        // Code the CTU as one 64x64 CU root (split recursion handles smaller sizes).
        code_cu(src, recon, &mut ctx, ref_pic, x0, y0, 6, 0, is_idr)?;
        // end_of_slice_segment_flag: 0 after every CTU but the last.
        if idx + 1 < total {
            ctx.enc.terminate(false);
        }
    }
    let mut cabac = ctx.enc.finish();
    let mut rbsp = header;
    rbsp.append(&mut cabac);
    let nal_type = if is_idr { NAL_IDR_N_LP } else { NAL_TRAIL_R };
    let nal = crate::params::assemble(nal_type, &rbsp);
    // In-loop deblocking over the whole picture, then the caller extends edges + publishes the DPB ref.
    let qp_y: Vec<u8> = ctx.qp_map;
    let maps = DeblockMaps {
        blocks_w: maps_w,
        blocks_h: maps_h,
        intra: ctx.intra_map,
        cbf: ctx.cbf_map,
        mv: ctx.mv_map,
        tu_edge_v: ctx.tu_v,
        tu_edge_h: ctx.tu_h,
        pu_edge_v: ctx.pu_v,
        pu_edge_h: ctx.pu_h,
    };
    deblock_picture(&mut recon.y, &mut recon.u, &mut recon.v, &qp_y, &maps)?;
    Ok(PictureResult { slice_nal: nal })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params;
    use crate::{Encoder, EncoderConfig, RateControl, YuvFrame};

    fn tiny_cfg(w: u32, h: u32) -> EncoderConfig {
        let mut c = EncoderConfig::new(w, h, 30, 1);
        c.rate = RateControl::Qp(32);
        c.keyint = 250;
        c.aq_strength = 0.0;
        c.scenecut = false;
        c
    }

    fn solid(w: usize, h: usize, y: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (vec![y; w * h], vec![128u8; w * h / 4], vec![128u8; w * h / 4])
    }

    fn annexb_split(stream: &[u8]) -> Vec<Vec<u8>> {
        let mut nals = Vec::new();
        let mut starts = Vec::new();
        let mut i = 0usize;
        while i + 4 <= stream.len() {
            if stream[i..].starts_with(&[0, 0, 0, 1]) {
                starts.push(i + 4);
                i += 4;
            } else {
                i += 1;
            }
        }
        for (k, s) in starts.iter().enumerate() {
            let e = starts.get(k + 1).map(|n| n - 4).unwrap_or(stream.len());
            nals.push(stream[*s..e].to_vec());
        }
        nals
    }

    #[test]
    fn idr_header_parses_in_our_decoder() {
        let mut enc = Encoder::new(tiny_cfg(64, 64)).unwrap();
        let (y, u, v) = solid(64, 64, 100);
        let pkts = enc.encode(&YuvFrame { y: &y, u: &u, v: &v, y_stride: 64, uv_stride: 32 }, 0).unwrap();
        assert_eq!(pkts.len(), 1);
        assert!(pkts[0].keyframe);
        let nals = annexb_split(&pkts[0].data);
        // AUD + VPS/SPS/PPS + SEI + slice.
        assert!(nals.len() >= 5, "got {} nals", nals.len());
        let slice = nals.last().unwrap();
        assert_eq!((slice[0] >> 1) & 0x3f, 20);
        // Parse the slice header with our decoder (SliceHeader::parse).
        let rbsp = filmcraft_bitstream::unescape_rbsp(&slice[2..]);
        let nal = filmcraft_hevc::slice::NalHeader::parse(slice).unwrap();
        let (vps, sps, pps) = enc.parameter_sets();
        let sps_parsed = std::sync::Arc::new(filmcraft_hevc::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(&sps[2..])).unwrap());
        let pps_parsed = std::sync::Arc::new(filmcraft_hevc::params::Pps::parse(&filmcraft_bitstream::unescape_rbsp(&pps[2..])).unwrap());
        let _ = vps;
        let (sh, _, _) = filmcraft_hevc::slice::SliceHeader::parse(&rbsp, nal, |_| Ok((pps_parsed.clone(), sps_parsed.clone())), None).unwrap();
        assert_eq!(sh.slice_type, filmcraft_hevc::slice::SliceType::I);
        assert_eq!(sh.poc_lsb, 0);
        assert_eq!(pps_parsed.init_qp + sh.qp_delta, 30); // Qp(32) IDR uses 32-2
        assert!(sh.first_slice_segment_in_pic);
        assert_eq!(params::NAL_IDR_N_LP, 20);
    }

    #[test]
    fn idr_plus_p_decode_to_two_pictures() {
        let mut enc = Encoder::new(tiny_cfg(64, 64)).unwrap();
        let (y0, u0, v0) = solid(64, 64, 90);
        let (y1, u1, v1) = solid(64, 64, 110);
        let mut pkts = enc.encode(&YuvFrame { y: &y0, u: &u0, v: &v0, y_stride: 64, uv_stride: 32 }, 0).unwrap();
        pkts.extend(enc.encode(&YuvFrame { y: &y1, u: &u1, v: &v1, y_stride: 64, uv_stride: 32 }, 1).unwrap());
        assert_eq!(pkts.len(), 2);
        let mut dec = filmcraft_hevc::Decoder::new();
        let mut pics = Vec::new();
        for (k, p) in pkts.iter().enumerate() {
            pics.extend(dec.decode(&p.data, k as i64).unwrap());
        }
        pics.extend(dec.flush());
        assert_eq!(pics.len(), 2);
        assert_eq!((pics[0].width, pics[0].height), (64, 64));
        assert_eq!((pics[1].width, pics[1].height), (64, 64));
        assert!(pics[0].key);
        assert!(!pics[1].key);
    }

    #[test]
    fn scan_orders_match_hand_positions() {
        // Diagonal 4x4 (6.5.3): first six and last are fixed by hand.
        let d = diag4_pos();
        assert_eq!(&d[..6], &[(0, 0), (0, 1), (1, 0), (0, 2), (1, 1), (2, 0)]);
        assert_eq!(d[15], (3, 3));
        // Horizontal vs vertical 4x4 differ at index 1.
        assert_eq!(pos_scan_4(1)[1], (1, 0));
        assert_eq!(pos_scan_4(2)[1], (0, 1));
        // Sub-block diagonal 2x2 matches the 4-entry diagonal.
        let sb = subblock_scan(1, 0);
        assert_eq!(sb, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
    }

    #[test]
    fn coding_is_deterministic() {
        let run = || {
            let mut enc = Encoder::new(tiny_cfg(64, 64)).unwrap();
            let (y, u, v) = solid(64, 64, 77);
            enc.encode(&YuvFrame { y: &y, u: &u, v: &v, y_stride: 64, uv_stride: 32 }, 0).unwrap()
        };
        let (a, b) = (run(), run());
        assert_eq!(a.len(), b.len());
        assert_eq!(a[0].data, b[0].data);
    }
}
