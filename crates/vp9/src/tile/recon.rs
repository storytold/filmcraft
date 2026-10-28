//! Residual syntax (6.4.21 - 6.4.26) with the prediction and reconstruction processes it
//! triggers (8.5, 8.6).

use super::{Blk, TileDecoder};
use crate::boolcoder::BoolDecoder;
use crate::error::Error;
use crate::frame::Mv;
use crate::inter::{self, McBlock, RefPlane};
use crate::intra::{self, IntraEdge};
use crate::tables::{
    BLOCK_8X8, CAT_PROBS, COEFBAND_4X4, COEFBAND_8X8PLUS, DCT_DCT, ENERGY_CLASS, EXTRA_BITS, INTERP_EXTEND, INTRA_FRAME, MAX_TXSIZE, MODE2TXFM, NUM_4X4_HIGH,
    NUM_4X4_WIDE, NUM_8X8_HIGH, NUM_8X8_WIDE, SS_SIZE_LOOKUP, TX_4X4, TX_32X32, neighbors, pareto_full, scan,
};
use crate::transform;

/// Sample rectangle of a block region within its plane.
#[derive(Clone, Copy)]
struct Region {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

/// round_mv_comp_q2 / q4 (8.5.2.1).
/// Location and transform parameters of one transform block within a plane.
#[derive(Clone, Copy)]
struct TxBlock {
    plane: usize,
    start_x: usize,
    start_y: usize,
    tx_sz: u8,
    tx_type: u8,
}

/// Walks the pareto part of the token tree, returning a token in `2..=10`.
fn read_large_token(bd: &mut BoolDecoder, pp: &[u8; 8]) -> usize {
    if !bd.read_bool(pp[0]) {
        if !bd.read_bool(pp[1]) {
            2
        } else if !bd.read_bool(pp[2]) {
            3
        } else {
            4
        }
    } else if !bd.read_bool(pp[3]) {
        if !bd.read_bool(pp[4]) { 5 } else { 6 }
    } else if !bd.read_bool(pp[5]) {
        if !bd.read_bool(pp[6]) { 7 } else { 8 }
    } else if !bd.read_bool(pp[7]) {
        9
    } else {
        10
    }
}

/// read_coef (6.4.26): the unsigned coefficient magnitude of a token of at least 2.
fn read_coef(bd: &mut BoolDecoder, token: usize, bit_depth: u8) -> i64 {
    let (cat, num_extra, base) = EXTRA_BITS[token];
    let mut v = base as i64;
    if token == 10 {
        for e in 0..(bit_depth as u32 - 8) {
            if bd.read_bool(255) {
                v += 1 << (5 + bit_depth as u32 - e);
            }
        }
    }
    let cp = CAT_PROBS[cat as usize];
    for e in 0..num_extra as usize {
        if bd.read_bool(cp[e]) {
            v += 1 << (num_extra as usize - 1 - e);
        }
    }
    v
}

fn round_q2(v: i32) -> i32 {
    (if v < 0 { v - 1 } else { v + 1 }) / 2
}
fn round_q4(v: i32) -> i32 {
    (if v < 0 { v - 2 } else { v + 2 }) / 4
}

impl<'a> TileDecoder<'a> {
    pub(super) fn residual(&mut self, bd: &mut BoolDecoder, b: &mut Blk) {
        let bsize = b.size.max(BLOCK_8X8) as usize;
        let (ssx, ssy) = (self.ss_x, self.ss_y);
        let uv_tx = if b.size < BLOCK_8X8 { 0 } else { b.tx_size.min(MAX_TXSIZE[SS_SIZE_LOOKUP[b.size as usize][ssx][ssy] as usize]) };
        for plane in 0..3 {
            let tx_sz = if plane > 0 { uv_tx } else { b.tx_size };
            let step = 1usize << tx_sz;
            let (sx, sy) = if plane > 0 { (ssx, ssy) } else { (0, 0) };
            let plane_sz = if plane > 0 { SS_SIZE_LOOKUP[bsize][ssx][ssy] as usize } else { bsize };
            let n4w = NUM_4X4_WIDE[plane_sz] as usize;
            let n4h = NUM_4X4_HIGH[plane_sz] as usize;
            let base_x = (b.c * 8) >> sx;
            let base_y = (b.r * 8) >> sy;
            if b.is_inter {
                if b.size < BLOCK_8X8 {
                    for y in 0..n4h {
                        for x in 0..n4w {
                            self.predict_inter(b, plane, Region { x: base_x + 4 * x, y: base_y + 4 * y, w: 4, h: 4 }, y * n4w + x);
                        }
                    }
                } else {
                    self.predict_inter(b, plane, Region { x: base_x, y: base_y, w: n4w * 4, h: n4h * 4 }, 0);
                }
            }
            let max_x = (self.mi_cols * 8) >> sx;
            let max_y = (self.mi_rows * 8) >> sy;
            let mut block_idx = 0;
            for y in (0..n4h).step_by(step) {
                for x in (0..n4w).step_by(step) {
                    let start_x = base_x + 4 * x;
                    let start_y = base_y + 4 * y;
                    let mut nonzero = false;
                    if start_x < max_x && start_y < max_y {
                        let tx_type = self.tx_type(b, plane, tx_sz, block_idx);
                        if !b.is_inter {
                            let mode = if plane > 0 {
                                b.uv_mode
                            } else if b.size >= BLOCK_8X8 {
                                b.y_mode
                            } else {
                                b.sub_modes[block_idx]
                            };
                            let edge = IntraEdge {
                                have_left: b.avail_l || x > 0,
                                have_above: b.avail_u || y > 0,
                                not_on_right: x + step < n4w,
                                tx_size: tx_sz,
                                max_x: max_x - 1,
                                max_y: max_y - 1,
                                bit_depth: self.bit_depth,
                            };
                            let (stride, x_off) = (self.strip.strides[plane], self.strip.x_off[plane]);
                            intra::predict(&mut self.strip.planes[plane], stride, x_off, start_x, start_y, mode, &edge);
                        }
                        if !b.skip {
                            let (eob, rows) = self.tokens(bd, b, &TxBlock { plane, start_x, start_y, tx_sz, tx_type });
                            nonzero = eob > 0;
                            b.eob_total += nonzero as u32;
                            if eob > 0 {
                                let (stride, x_off) = (self.strip.strides[plane], self.strip.x_off[plane]);
                                let o = start_y * stride + start_x - x_off;
                                transform::inverse_transform_add(
                                    &self.coefs,
                                    &transform::InverseTx { tx_size: tx_sz, tx_type, lossless: self.h.lossless, eob, rows, bit_depth: self.bit_depth },
                                    &mut self.strip.planes[plane][o..],
                                    stride,
                                );
                                let sc = scan(tx_sz, tx_type);
                                for &p in &sc[..eob] {
                                    self.coefs[p as usize] = 0;
                                }
                            }
                        }
                    }
                    let ax = start_x >> 2;
                    let ly = start_y >> 2;
                    let an = &mut self.above_nonzero[plane];
                    let e = (ax + step).min(an.len());
                    an[ax.min(e)..e].fill(nonzero as u8);
                    let ln = &mut self.left_nonzero[plane];
                    let e = (ly + step).min(ln.len());
                    ln[ly.min(e)..e].fill(nonzero as u8);
                    block_idx += 1;
                }
            }
        }
    }

    /// Transform type (6.4.25).
    fn tx_type(&self, b: &Blk, plane: usize, tx_sz: u8, block_idx: usize) -> u8 {
        if plane > 0 || tx_sz == TX_32X32 {
            DCT_DCT
        } else if tx_sz == TX_4X4 {
            if self.h.lossless || b.is_inter { DCT_DCT } else { MODE2TXFM[if b.size < BLOCK_8X8 { b.sub_modes[block_idx] } else { b.y_mode } as usize] }
        } else {
            MODE2TXFM[b.y_mode as usize]
        }
    }

    /// Above/left non-zero context of a transform block (the initial token context).
    fn initial_token_ctx(&self, plane: usize, start_x: usize, start_y: usize, tx_sz: u8) -> usize {
        let (sx, sy) = if plane > 0 { (self.ss_x, self.ss_y) } else { (0, 0) };
        let max_x4 = (2 * self.mi_cols) >> sx;
        let max_y4 = (2 * self.mi_rows) >> sy;
        let n = 1usize << tx_sz;
        let (x4, y4) = (start_x >> 2, start_y >> 2);
        let mut above = 0u8;
        let mut left = 0u8;
        for i in 0..n {
            if x4 + i < max_x4 {
                above |= self.above_nonzero[plane][x4 + i];
            }
            if y4 + i < max_y4 {
                left |= self.left_nonzero[plane][y4 + i];
            }
        }
        (above + left) as usize
    }

    /// tokens (6.4.24): reads and dequantizes the coefficients into `self.coefs`. Returns the end
    /// of block position and the number of leading coefficient rows that may be non-zero.
    fn tokens(&mut self, bd: &mut BoolDecoder, b: &Blk, t: &TxBlock) -> (usize, usize) {
        let TxBlock { plane, start_x, start_y, tx_sz, tx_type } = *t;
        let seg_eob = 16usize << (tx_sz << 1);
        let sc = scan(tx_sz, tx_type);
        let nb = neighbors(tx_sz, tx_type);
        let bands: &[u8] = if tx_sz == 0 { &COEFBAND_4X4 } else { &COEFBAND_8X8PLUS };
        let ptype = (plane > 0) as usize;
        let rtype = b.is_inter as usize;
        let probs = &self.fc.coef[tx_sz as usize][ptype][rtype];
        let ctx0 = self.initial_token_ctx(plane, start_x, start_y, tx_sz);
        let q = self.s.seg_q[b.seg_id as usize][ptype];
        let dq_shift = (tx_sz == TX_32X32) as u32;
        let pareto = pareto_full();
        let counting = self.s.counting;
        let log2n = 2 + tx_sz as usize;
        let bit_depth = self.bit_depth;
        let mut max_row = 0usize;
        let mut check_eob = true;
        let mut c = 0usize;
        let tc = &mut self.token_cache;
        let coefs = &mut self.coefs;
        let counts = &mut self.strip.counts;
        while c < seg_eob {
            let pos = sc[c] as usize;
            let band = bands[c] as usize;
            let ctx = if c == 0 {
                ctx0
            } else {
                let (a, bb) = nb[c];
                ((1 + tc[a as usize] + tc[bb as usize]) >> 1) as usize
            };
            let p = &probs[band][ctx];
            if check_eob {
                let more = bd.read_bool(p[0]);
                if counting {
                    counts.more_coefs[tx_sz as usize][ptype][rtype][band][ctx][more as usize] += 1;
                }
                if !more {
                    break;
                }
            }
            if !bd.read_bool(p[1]) {
                tc[pos] = 0;
                if counting {
                    counts.token[tx_sz as usize][ptype][rtype][band][ctx][0] += 1;
                }
                check_eob = false;
                c += 1;
                continue;
            }
            check_eob = true;
            let token: usize;
            let coef: i64;
            if !bd.read_bool(p[2]) {
                if counting {
                    counts.token[tx_sz as usize][ptype][rtype][band][ctx][1] += 1;
                }
                token = 1;
                coef = 1;
            } else {
                if counting {
                    counts.token[tx_sz as usize][ptype][rtype][band][ctx][2] += 1;
                }
                let pp = &pareto[p[2].max(1) as usize - 1];
                token = read_large_token(bd, pp);
                coef = read_coef(bd, token, bit_depth);
            }
            tc[pos] = ENERGY_CLASS[token];
            let sign = bd.read_bool(128);
            let qv = if pos == 0 { q[0] } else { q[1] } as i64;
            let d = ((coef * qv) >> dq_shift) as i32;
            coefs[pos] = if sign { d.wrapping_neg() } else { d };
            max_row = max_row.max(pos >> log2n);
            c += 1;
        }
        (c, max_row + 1)
    }

    /// Inter prediction process (8.5.2) for one block region.
    fn predict_inter(&mut self, b: &Blk, plane: usize, region: Region, block_idx: usize) {
        let Region { x, y, w, h } = region;
        let is_compound = b.ref_frame[1] > INTRA_FRAME;
        let (sx, sy) = if plane > 0 { (self.ss_x, self.ss_y) } else { (0, 0) };
        for rl in 0..1 + is_compound as usize {
            // Motion vector selection (8.5.2.1).
            let bm = &b.block_mvs[rl];
            let mv = if plane == 0 || b.size >= BLOCK_8X8 || (sx == 0 && sy == 0) {
                bm[block_idx]
            } else if sx == 0 && sy == 1 {
                Mv::new(round_q2(bm[block_idx].row as i32 + bm[block_idx + 2].row as i32), round_q2(bm[block_idx].col as i32 + bm[block_idx + 2].col as i32))
            } else if sx == 1 && sy == 0 {
                Mv::new(round_q2(bm[block_idx].row as i32 + bm[block_idx + 1].row as i32), round_q2(bm[block_idx].col as i32 + bm[block_idx + 1].col as i32))
            } else {
                let sr: i32 = bm.iter().map(|m| m.row as i32).sum();
                let sc: i32 = bm.iter().map(|m| m.col as i32).sum();
                Mv::new(round_q4(sr), round_q4(sc))
            };
            // Motion vector clamping (8.5.2.2).
            let bh = NUM_8X8_HIGH[b.size as usize] as i32;
            let bw = NUM_8X8_WIDE[b.size as usize] as i32;
            let (r, c) = (b.r as i32, b.c as i32);
            let to_top = (-(r * 8 * 16)) >> sy;
            let to_bottom = (((self.mi_rows as i32 - bh - r) * 8) * 16) >> sy;
            let to_left = (-(c * 8 * 16)) >> sx;
            let to_right = (((self.mi_cols as i32 - bw - c) * 8) * 16) >> sx;
            let spel_left = (INTERP_EXTEND + ((bw * 8) >> sx)) << 4;
            let spel_right = spel_left - 16;
            let spel_top = (INTERP_EXTEND + ((bh * 8) >> sy)) << 4;
            let spel_bottom = spel_top - 16;
            let cmv_row = ((2 * mv.row as i32) >> sy).clamp(to_top - spel_top, (to_bottom + spel_bottom).max(to_top - spel_top));
            let cmv_col = ((2 * mv.col as i32) >> sx).clamp(to_left - spel_left, (to_right + spel_right).max(to_left - spel_left));
            let ri = (b.ref_frame[rl] - 1).clamp(0, 2) as usize;
            let Some(rf) = &self.s.refs[ri] else {
                if self.error.is_none() {
                    self.error = Some(Error::MissingReference(format!("block uses unusable reference {}", b.ref_frame[rl])));
                }
                let out = if rl == 0 { &mut self.pred } else { &mut self.pred2 };
                out[..w * h].fill(1 << (self.bit_depth - 1));
                continue;
            };
            // Motion vector scaling (8.5.2.3).
            let (x_scale, y_scale) = (rf.x_scale as i64, rf.y_scale as i64);
            let (start_x, start_y, step_x, step_y) = if rf.x_step == 16 && rf.y_step == 16 && x_scale == 1 << 14 && y_scale == 1 << 14 {
                (((x as i32) << 4) + cmv_col, ((y as i32) << 4) + cmv_row, 16, 16)
            } else {
                self.strip.scaled_blocks += 1;
                let base_x = (x as i64 * x_scale) >> 14;
                let base_y = (y as i64 * y_scale) >> 14;
                let luma_x = (x << sx) as i64;
                let luma_y = (y << sy) as i64;
                let frac_x = ((16 * luma_x * x_scale) >> 14) & 15;
                let frac_y = ((16 * luma_y * y_scale) >> 14) & 15;
                let dx = ((cmv_col as i64 * x_scale) >> 14) + frac_x;
                let dy = ((cmv_row as i64 * y_scale) >> 14) + frac_y;
                (((base_x << 4) + dx) as i32, ((base_y << 4) + dy) as i32, rf.x_step, rf.y_step)
            };
            let f = rf.frame;
            let rp =
                RefPlane { frame: f, plane, last_x: ((f.info.width as i32 + sx as i32) >> sx) - 1, last_y: ((f.info.height as i32 + sy as i32) >> sy) - 1 };
            let out = if rl == 0 { &mut self.pred } else { &mut self.pred2 };
            let blk = McBlock { x: start_x, y: start_y, step_x, step_y, w, h, filter: b.interp_filter, bit_depth: self.bit_depth };
            inter::predict(&rp, &blk, out, &mut self.mc_tmp, &mut self.mc_win);
        }
        let (stride, x_off) = (self.strip.strides[plane], self.strip.x_off[plane]);
        let dst = &mut self.strip.planes[plane];
        for i in 0..h {
            let o = (y + i) * stride + x - x_off;
            let row = &mut dst[o..o + w];
            if is_compound {
                for ((d, &a), &bb) in row.iter_mut().zip(&self.pred[i * w..i * w + w]).zip(&self.pred2[i * w..i * w + w]) {
                    *d = ((a as u32 + bb as u32 + 1) >> 1) as u16;
                }
            } else {
                row.copy_from_slice(&self.pred[i * w..i * w + w]);
            }
        }
    }
}
