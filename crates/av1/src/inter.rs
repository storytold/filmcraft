//! Inter prediction (spec 7.11.3): motion vector scaling, block inter prediction, warped
//! motion, masks (wedge, difference weight, inter-intra), distance weights and OBMC.

use std::sync::OnceLock;

use crate::frame::Plane;
use crate::mvpred::round2_signed64;
use crate::spec_tables::{
    BLOCK_8X8, BLOCK_SIZES, COMPOUND_AVERAGE, COMPOUND_DIFFWTD, COMPOUND_DISTANCE, COMPOUND_INTRA, COMPOUND_WEDGE, DIV_LUT, DIV_LUT_BITS, DIV_LUT_PREC_BITS,
    EIGHTTAP, EIGHTTAP_SHARP, EIGHTTAP_SMOOTH, FILTER_BITS, GLOBAL_GLOBALMV, GLOBALMV, II_H_PRED, II_SMOOTH_PRED, II_V_PRED, II_WEIGHTS_1D, INTRA_FRAME,
    LAST_FRAME, LOCALWARP, LS_MV_MAX, MASK_MASTER_SIZE, MAX_FRAME_DISTANCE, MAX_SB_SIZE, MI_HEIGHT_LOG2, MI_WIDTH_LOG2, NUM_4X4_BLOCKS_HIGH,
    NUM_4X4_BLOCKS_WIDE, OBMC, OBMC_MASK_2, OBMC_MASK_4, OBMC_MASK_8, OBMC_MASK_16, OBMC_MASK_32, QUANT_DIST_LOOKUP, QUANT_DIST_WEIGHT, REF_SCALE_SHIFT,
    SCALE_SUBPEL_BITS, SUBPEL_BITS, SUBPEL_FILTERS, SUBPEL_MASK, TRANSLATION, WARP_PARAM_REDUCE_BITS, WARPED_FILTERS, WARPEDDIFF_PREC_BITS,
    WARPEDMODEL_NONDIAGAFFINE_CLAMP, WARPEDMODEL_PREC_BITS, WARPEDMODEL_TRANS_CLAMP, WARPEDPIXEL_PREC_SHIFTS, WEDGE_BITS, WEDGE_CODEBOOK, WEDGE_HORIZONTAL,
    WEDGE_MASTER_OBLIQUE_EVEN, WEDGE_MASTER_OBLIQUE_ODD, WEDGE_MASTER_VERTICAL, WEDGE_OBLIQUE27, WEDGE_OBLIQUE63, WEDGE_OBLIQUE117, WEDGE_OBLIQUE153,
    WEDGE_TYPES, WEDGE_VERTICAL,
};
use crate::tile::TileDecoder;

const PRED_STRIDE: usize = 128;

/// A w x h region of `plane` at (x, y) that inter prediction fills.
#[derive(Clone, Copy)]
pub(crate) struct PredBlock {
    pub plane: usize,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// Position (1/1024-sample units) and step of a block in a reference plane, from the motion vector scaling process.
#[derive(Clone, Copy)]
struct McSource {
    plane: usize,
    ref_idx: i32,
    x: i32,
    y: i32,
    x_step: i32,
    y_step: i32,
}

/// Start position (1/1024-sample units) and size of a block predicted without reference scaling.
#[derive(Clone, Copy)]
struct McBlock {
    x: i32,
    y: i32,
    w: usize,
    h: usize,
}

/// Warp used to predict a block from one reference.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WarpKind {
    None,
    Local,
    Global,
}

#[inline(always)]
fn round2(x: i32, n: u32) -> i32 {
    if n == 0 { x } else { (x + (1 << (n - 1))) >> n }
}

/// Rounding variables (7.11.3.2): (InterRound0, InterRound1, InterPostRound).
pub(crate) fn rounding(is_compound: bool, bd: u32) -> (u32, u32, u32) {
    let mut r0 = 3;
    let mut r1 = if is_compound { 7 } else { 11 };
    if bd == 12 {
        r0 += 2;
        if !is_compound {
            r1 -= 2;
        }
    }
    (r0, r1, 2 * FILTER_BITS as u32 - (r0 + r1))
}

/// Resolve divisor process (7.11.3.7): (divShift, divFactor).
fn resolve_divisor(d: i64) -> (i32, i64) {
    let a = d.unsigned_abs();
    let n = 63 - a.leading_zeros() as i32;
    let e = a as i64 - (1i64 << n);
    let f = if n > DIV_LUT_BITS as i32 { round2_signed64(e, (n - DIV_LUT_BITS as i32) as u32) } else { e << (DIV_LUT_BITS as i32 - n) };
    let shift = n + DIV_LUT_PREC_BITS as i32;
    let factor = DIV_LUT[f as usize] as i64;
    (shift, if d < 0 { -factor } else { factor })
}

/// Setup shear process (7.11.3.6): (warpValid, alpha, beta, gamma, delta).
pub(crate) fn setup_shear(p: &[i32; 6]) -> (bool, i32, i32, i32, i32) {
    let prec = 1i64 << WARPEDMODEL_PREC_BITS;
    let alpha0 = (p[2] as i64 - prec).clamp(-32768, 32767);
    let beta0 = (p[3] as i64).clamp(-32768, 32767);
    let (div_shift, div_factor) = resolve_divisor(p[2] as i64);
    let v = (p[4] as i64) << WARPEDMODEL_PREC_BITS;
    let gamma0 = round2_signed_wide(v as i128 * div_factor as i128, div_shift as u32).clamp(-32768, 32767);
    let w = p[3] as i64 * p[4] as i64;
    let delta0 = (p[5] as i64 - round2_signed_wide(w as i128 * div_factor as i128, div_shift as u32) - prec).clamp(-32768, 32767);
    let rb = WARP_PARAM_REDUCE_BITS as u32;
    let red = |x: i64| (round2_signed64(x, rb) << rb) as i32;
    let (alpha, beta, gamma, delta) = (red(alpha0), red(beta0), red(gamma0), red(delta0));
    let valid = !(4 * alpha.abs() + 7 * beta.abs() >= (1 << WARPEDMODEL_PREC_BITS) || 4 * gamma.abs() + 4 * delta.abs() >= (1 << WARPEDMODEL_PREC_BITS));
    (valid, alpha, beta, gamma, delta)
}

fn round2_signed_wide(x: i128, n: u32) -> i64 {
    if n == 0 {
        return x as i64;
    }
    (if x >= 0 { (x + (1 << (n - 1))) >> n } else { -((-x + (1 << (n - 1))) >> n) }) as i64
}

/// WedgeMasks[ bsize ][ flipSign ][ wedge ] (7.11.3.11), stored as w*h bytes each.
fn wedge_masks() -> &'static Vec<Vec<[Vec<u8>; 2]>> {
    static M: OnceLock<Vec<Vec<[Vec<u8>; 2]>>> = OnceLock::new();
    M.get_or_init(|| {
        let n = MASK_MASTER_SIZE;
        let mut master = vec![[[0u8; 64]; 64]; 6];
        for j in 0..n {
            let mut shift = (n / 4) as i32;
            let mut i = 0;
            while i < n {
                master[WEDGE_OBLIQUE63][i][j] = WEDGE_MASTER_OBLIQUE_EVEN[(j as i32 - shift).clamp(0, n as i32 - 1) as usize];
                shift -= 1;
                master[WEDGE_OBLIQUE63][i + 1][j] = WEDGE_MASTER_OBLIQUE_ODD[(j as i32 - shift).clamp(0, n as i32 - 1) as usize];
                master[WEDGE_VERTICAL][i][j] = WEDGE_MASTER_VERTICAL[j];
                master[WEDGE_VERTICAL][i + 1][j] = WEDGE_MASTER_VERTICAL[j];
                i += 2;
            }
        }
        for i in 0..n {
            for j in 0..n {
                let msk = master[WEDGE_OBLIQUE63][i][j];
                master[WEDGE_OBLIQUE27][j][i] = msk;
                master[WEDGE_OBLIQUE117][i][n - 1 - j] = 64 - msk;
                master[WEDGE_OBLIQUE153][n - 1 - j][i] = 64 - msk;
                master[WEDGE_HORIZONTAL][j][i] = master[WEDGE_VERTICAL][i][j];
            }
        }
        let mut out = vec![Vec::new(); BLOCK_SIZES];
        for bsize in BLOCK_8X8..BLOCK_SIZES {
            if WEDGE_BITS[bsize] == 0 {
                continue;
            }
            let w = 4 * NUM_4X4_BLOCKS_WIDE[bsize] as usize;
            let h = 4 * NUM_4X4_BLOCKS_HIGH[bsize] as usize;
            let w4 = NUM_4X4_BLOCKS_WIDE[bsize];
            let h4 = NUM_4X4_BLOCKS_HIGH[bsize];
            let shape = if h4 > w4 {
                0
            } else if h4 < w4 {
                1
            } else {
                2
            };
            for wedge in 0..WEDGE_TYPES {
                let cb = &WEDGE_CODEBOOK[shape][wedge];
                let dir = cb[0] as usize;
                let xoff = n / 2 - ((cb[1] as usize * w) >> 3);
                let yoff = n / 2 - ((cb[2] as usize * h) >> 3);
                let mut sum = 0usize;
                for i in 0..w {
                    sum += master[dir][yoff][xoff + i] as usize;
                }
                for i in 1..h {
                    sum += master[dir][yoff + i][xoff] as usize;
                }
                let avg = (sum + (w + h - 1) / 2) / (w + h - 1);
                let flip = (avg < 32) as usize;
                let mut a = vec![0u8; w * h];
                let mut b = vec![0u8; w * h];
                for i in 0..h {
                    for j in 0..w {
                        let m = master[dir][yoff + i][xoff + j];
                        a[i * w + j] = m;
                        b[i * w + j] = 64 - m;
                    }
                }
                let pair = if flip == 0 { [a, b] } else { [b, a] };
                out[bsize].push(pair);
            }
        }
        out
    })
}

impl TileDecoder<'_, '_> {
    /// Reference plane and its (lastX, lastY) for refIdx (-1: the current frame for intra BC).
    fn ref_plane_dims(&self, ref_idx: i32, plane: usize) -> (i32, i32) {
        let (sx, sy) = if plane > 0 { (self.fs.ssx as i32, self.fs.ssy as i32) } else { (0, 0) };
        let (uw, fh) = if ref_idx < 0 {
            ((self.fs.fh.mi_cols * 4) as i32, (self.fs.fh.mi_rows * 4) as i32)
        } else {
            let ri = &self.fs.ref_info[ref_idx as usize];
            (ri.upscaled_width as i32, ri.frame_height as i32)
        };
        (((uw + sx) >> sx) - 1, ((fh + sy) >> sy) - 1)
    }

    /// Motion vector scaling process (7.11.3.3): (startX, startY, stepX, stepY).
    fn mv_scaling(&self, plane: usize, ref_idx: i32, x: i32, y: i32, mv: [i32; 2]) -> (i32, i32, i32, i32) {
        let fh = &self.fs.fh;
        let (ref_uw, ref_h) = if ref_idx < 0 {
            (fh.upscaled_width as i64, fh.frame_height as i64)
        } else {
            let ri = &self.fs.ref_info[ref_idx as usize];
            (ri.upscaled_width as i64, ri.frame_height as i64)
        };
        let fw = fh.frame_width as i64;
        let fhh = fh.frame_height as i64;
        let rs = REF_SCALE_SHIFT as i64;
        let x_scale = ((ref_uw << rs) + fw / 2) / fw;
        let y_scale = ((ref_h << rs) + fhh / 2) / fhh;
        let (sx, sy) = if plane > 0 { (self.fs.ssx as i32, self.fs.ssy as i32) } else { (0, 0) };
        let half = 1i64 << (SUBPEL_BITS - 1);
        let orig_x = ((x as i64) << SUBPEL_BITS) + ((2 * mv[1]) >> sx) as i64 + half;
        let orig_y = ((y as i64) << SUBPEL_BITS) + ((2 * mv[0]) >> sy) as i64 + half;
        let base_x = orig_x * x_scale - (half << rs);
        let base_y = orig_y * y_scale - (half << rs);
        let off = (1i64 << (SCALE_SUBPEL_BITS - SUBPEL_BITS)) / 2;
        let sh = (REF_SCALE_SHIFT + SUBPEL_BITS - SCALE_SUBPEL_BITS) as u32;
        let start_x = round2_signed64(base_x, sh) + off;
        let start_y = round2_signed64(base_y, sh) + off;
        let step_x = round2_signed64(x_scale, (REF_SCALE_SHIFT - SCALE_SUBPEL_BITS) as u32);
        let step_y = round2_signed64(y_scale, (REF_SCALE_SHIFT - SCALE_SUBPEL_BITS) as u32);
        (start_x as i32, start_y as i32, step_x as i32, step_y as i32)
    }

    fn ref_plane(&self, ref_idx: i32, plane: usize) -> &Plane {
        // A missing reference (rejected earlier for valid streams) predicts from the current frame.
        match usize::try_from(ref_idx).ok().and_then(|i| self.fs.refs.get(i)).and_then(|r| r.as_ref()) {
            Some(r) => &r.buf.planes[plane],
            None => &self.t.cur.planes[plane],
        }
    }

    /// Block inter prediction process (7.11.3.4) into `pred` (stride PRED_STRIDE); `tmp` holds
    /// the intermediate (horizontally filtered) rows.
    fn block_inter_prediction(&self, src: McSource, (w, h): (usize, usize), filters: [u8; 2], rnd: (u32, u32, u32), pred: &mut [i32], tmp: &mut [i32]) {
        let McSource { plane, ref_idx, x, y, x_step, y_step } = src;
        let refp = self.ref_plane(ref_idx, plane);
        let (last_x, last_y) = self.ref_plane_dims(ref_idx, plane);
        let mut fh = filters[1] as usize;
        if w <= 4 {
            if fh == EIGHTTAP || fh == EIGHTTAP_SHARP {
                fh = 4;
            } else if fh == EIGHTTAP_SMOOTH {
                fh = 5;
            }
        }
        let mut fv = filters[0] as usize;
        if h <= 4 {
            if fv == EIGHTTAP || fv == EIGHTTAP_SHARP {
                fv = 4;
            } else if fv == EIGHTTAP_SMOOTH {
                fv = 5;
            }
        }
        let unit = 1 << SCALE_SUBPEL_BITS;
        if x_step == unit && y_step == unit {
            let hf = &SUBPEL_FILTERS[fh][((x >> 6) & SUBPEL_MASK as i32) as usize];
            let vf = &SUBPEL_FILTERS[fv][((y >> 6) & SUBPEL_MASK as i32) as usize];
            mc_unscaled(refp, (last_x, last_y), McBlock { x, y, w, h }, (hf, vf), rnd, pred, tmp);
            return;
        }
        let inter_h = ((((h as i32 - 1) * y_step + (1 << SCALE_SUBPEL_BITS) - 1) >> SCALE_SUBPEL_BITS) + 8) as usize;
        for r in 0..inter_h {
            let ry = ((y >> 10) + r as i32 - 3).clamp(0, last_y) as usize;
            let row = refp.row(ry);
            for c in 0..w {
                let p = x + x_step * c as i32;
                let filt = &SUBPEL_FILTERS[fh][((p >> 6) & SUBPEL_MASK as i32) as usize];
                let mut s = 0i32;
                let bx = (p >> 10) - 3;
                for t in 0..8 {
                    s += filt[t] as i32 * row[(bx + t as i32).clamp(0, last_x) as usize] as i32;
                }
                tmp[r * w + c] = round2(s, rnd.0);
            }
        }
        for r in 0..h {
            for c in 0..w {
                let p = (y & 1023) + y_step * r as i32;
                let filt = &SUBPEL_FILTERS[fv][((p >> 6) & SUBPEL_MASK as i32) as usize];
                let base = (p >> 10) as usize;
                let mut s = 0i32;
                for t in 0..8 {
                    s += filt[t] as i32 * tmp[(base + t) * w + c];
                }
                pred[r * PRED_STRIDE + c] = round2(s, rnd.1);
            }
        }
    }

    /// Block warp process (7.11.3.5) for the 8x8 section (i8, j8).
    fn block_warp(&self, params: &[i32; 6], ref_idx: i32, blk: PredBlock, (i8, j8): (usize, usize), rnd: (u32, u32, u32), pred: &mut [i32]) {
        let PredBlock { plane, x, y, w, h } = blk;
        let (x, y) = (x as i32, y as i32);
        let refp = self.ref_plane(ref_idx, plane);
        let (last_x, last_y) = self.ref_plane_dims(ref_idx, plane);
        let (sx, sy) = if plane > 0 { (self.fs.ssx as i32, self.fs.ssy as i32) } else { (0, 0) };
        let src_x = (x + j8 as i32 * 8 + 4) << sx;
        let src_y = (y + i8 as i32 * 8 + 4) << sy;
        let dst_x = params[2] as i64 * src_x as i64 + params[3] as i64 * src_y as i64 + params[0] as i64;
        let dst_y = params[4] as i64 * src_x as i64 + params[5] as i64 * src_y as i64 + params[1] as i64;
        let (_, alpha, beta, gamma, delta) = setup_shear(params);
        let x4 = dst_x >> sx;
        let y4 = dst_y >> sy;
        let ix4 = (x4 >> WARPEDMODEL_PREC_BITS) as i32;
        let sx4 = (x4 & ((1 << WARPEDMODEL_PREC_BITS) - 1)) as i32;
        let iy4 = (y4 >> WARPEDMODEL_PREC_BITS) as i32;
        let sy4 = (y4 & ((1 << WARPEDMODEL_PREC_BITS) - 1)) as i32;
        let mut inter = [[0i32; 8]; 15];
        for i1 in -7..8i32 {
            let row = refp.row((iy4 + i1).clamp(0, last_y) as usize);
            for i2 in -4..4i32 {
                let sxv = sx4 + alpha * i2 + beta * i1;
                let offs = (round2(sxv, WARPEDDIFF_PREC_BITS as u32) + WARPEDPIXEL_PREC_SHIFTS as i32) as usize;
                let mut s = 0i32;
                for i3 in 0..8i32 {
                    s += WARPED_FILTERS[offs][i3 as usize] as i32 * row[(ix4 + i2 - 3 + i3).clamp(0, last_x) as usize] as i32;
                }
                inter[(i1 + 7) as usize][(i2 + 4) as usize] = round2(s, rnd.0);
            }
        }
        let lim_i = 4.min(h as i32 - i8 as i32 * 8 - 4);
        let lim_j = 4.min(w as i32 - j8 as i32 * 8 - 4);
        for i1 in -4..lim_i {
            for i2 in -4..lim_j {
                let syv = sy4 + gamma * i2 + delta * i1;
                let offs = (round2(syv, WARPEDDIFF_PREC_BITS as u32) + WARPEDPIXEL_PREC_SHIFTS as i32) as usize;
                let mut s = 0i32;
                for i3 in 0..8 {
                    s += WARPED_FILTERS[offs][i3] as i32 * inter[(i1 + i3 as i32 + 4) as usize][(i2 + 4) as usize];
                }
                pred[(i8 * 8 + (i1 + 4) as usize) * PRED_STRIDE + j8 * 8 + (i2 + 4) as usize] = round2(s, rnd.1);
            }
        }
    }

    /// Warp estimation process (7.11.3.8): sets LocalWarpParams / LocalValid.
    pub(crate) fn warp_estimation(&mut self) {
        let mut a = [[0i64; 2]; 2];
        let mut bx = [0i64; 2];
        let mut by = [0i64; 2];
        let mid_y = self.b.mi_row as i64 * 4 + self.b.bh4 as i64 * 2 - 1;
        let mid_x = self.b.mi_col as i64 * 4 + self.b.bw4 as i64 * 2 - 1;
        let suy = mid_y * 8;
        let sux = mid_x * 8;
        let duy = suy + self.b.mv_i[0][0] as i64;
        let dux = sux + self.b.mv_i[0][1] as i64;
        let ls = |a: i64, b: i64| ((a * b) >> 2) + (a + b);
        for i in 0..self.mvs.num_samples {
            let c = self.mvs.cand_list[i];
            let sy = c[0] as i64 - suy;
            let sx = c[1] as i64 - sux;
            let dy = c[2] as i64 - duy;
            let dx = c[3] as i64 - dux;
            if (sx - dx).abs() < LS_MV_MAX as i64 && (sy - dy).abs() < LS_MV_MAX as i64 {
                a[0][0] += ls(sx, sx) + 8;
                a[0][1] += ls(sx, sy) + 4;
                a[1][1] += ls(sy, sy) + 8;
                bx[0] += ls(sx, dx) + 8;
                bx[1] += ls(sy, dx) + 4;
                by[0] += ls(sx, dy) + 4;
                by[1] += ls(sy, dy) + 8;
            }
        }
        let det = a[0][0] * a[1][1] - a[0][1] * a[0][1];
        self.b.local_valid = det != 0;
        if det == 0 {
            return;
        }
        let (mut div_shift, mut div_factor) = resolve_divisor(det);
        div_shift -= WARPEDMODEL_PREC_BITS as i32;
        if div_shift < 0 {
            div_factor <<= -div_shift;
            div_shift = 0;
        }
        let prec = 1i64 << WARPEDMODEL_PREC_BITS;
        let cl = WARPEDMODEL_NONDIAGAFFINE_CLAMP as i64;
        let nondiag = |v: i64| round2_signed_wide(v as i128 * div_factor as i128, div_shift as u32).clamp(-cl + 1, cl - 1);
        let diag = |v: i64| round2_signed_wide(v as i128 * div_factor as i128, div_shift as u32).clamp(prec - cl + 1, prec + cl - 1);
        let mut p = [0i64; 6];
        p[2] = diag(a[1][1] * bx[0] - a[0][1] * bx[1]);
        p[3] = nondiag(-a[0][1] * bx[0] + a[0][0] * bx[1]);
        p[4] = nondiag(a[1][1] * by[0] - a[0][1] * by[1]);
        p[5] = diag(-a[0][1] * by[0] + a[0][0] * by[1]);
        let mvx = self.b.mv_i[0][1] as i64;
        let mvy = self.b.mv_i[0][0] as i64;
        let vx = mvx * (1 << (WARPEDMODEL_PREC_BITS - 3)) - (mid_x * (p[2] - prec) + mid_y * p[3]);
        let vy = mvy * (1 << (WARPEDMODEL_PREC_BITS - 3)) - (mid_x * p[4] + mid_y * (p[5] - prec));
        let tc = WARPEDMODEL_TRANS_CLAMP as i64;
        p[0] = vx.clamp(-tc, tc - 1);
        p[1] = vy.clamp(-tc, tc - 1);
        for i in 0..6 {
            self.b.local_warp_params[i] = p[i] as i32;
        }
    }

    /// Inter prediction process (7.11.3.1) for the region `blk`, using the motion of the block at (cand_row, cand_col).
    pub(crate) fn predict_inter(&mut self, blk: PredBlock, cand_row: usize, cand_col: usize) {
        let bd = self.fs.bit_depth;
        let mi_i = self.t.mi.idx(cand_row, cand_col);
        let cand_refs = self.t.mi.ref_frame[mi_i];
        let is_compound = cand_refs[1] > INTRA_FRAME as i8;
        let rnd = rounding(is_compound, bd);
        if blk.plane == 0 && self.b.motion_mode as usize == LOCALWARP {
            self.update_local_warp_validity();
        }
        let mut preds = [std::mem::take(&mut self.pred_buf[0]), std::mem::take(&mut self.pred_buf[1])];
        let mut tmp = std::mem::take(&mut self.mc_tmp);
        for ref_list in 0..1 + is_compound as usize {
            self.predict_from_reference(blk, mi_i, ref_list, rnd, &mut preds[ref_list], &mut tmp);
        }
        let ct = self.b.compound_type;
        if ct == COMPOUND_WEDGE && blk.plane == 0 {
            self.fill_wedge_mask(blk);
        } else if ct == COMPOUND_INTRA {
            self.fill_interintra_mask(blk);
        } else if ct == COMPOUND_DIFFWTD && blk.plane == 0 {
            self.fill_diffwtd_mask(blk, &preds, (bd - 8) + rnd.2);
        }
        self.store_prediction(blk, &preds, cand_refs, rnd.2);
        self.pred_buf = preds;
        self.mc_tmp = tmp;
        if self.b.motion_mode as usize == OBMC {
            self.overlapped_motion_compensation(blk.plane, blk.w, blk.h);
        }
    }

    /// Re-estimates the local warp model for the block and records whether it is usable.
    fn update_local_warp_validity(&mut self) {
        self.warp_estimation();
        if self.b.local_valid {
            let p = self.b.local_warp_params;
            self.b.local_valid = setup_shear(&p).0;
        }
    }

    /// Which warp (if any) predicts `blk` from `ref_frame`.
    fn select_warp(&self, blk: PredBlock, ref_frame: i8, ref_idx: i32) -> WarpKind {
        let fh = &self.fs.fh;
        let ym = self.b.y_mode;
        let global = (ym == GLOBALMV || ym == GLOBAL_GLOBALMV) && ref_frame > INTRA_FRAME as i8 && fh.gm_type[ref_frame as usize] as usize > TRANSLATION;
        let global_valid = global && setup_shear(&fh.gm_params[ref_frame as usize]).0;
        if blk.w < 8 || blk.h < 8 || fh.force_integer_mv {
            WarpKind::None
        } else if self.b.motion_mode as usize == LOCALWARP && self.b.local_valid {
            WarpKind::Local
        } else if global && !self.is_scaled_ref(ref_idx) && global_valid {
            WarpKind::Global
        } else {
            WarpKind::None
        }
    }

    /// Predicts `blk` from one reference list of the candidate at `mi_i` into `pred`.
    fn predict_from_reference(&self, blk: PredBlock, mi_i: usize, ref_list: usize, rnd: (u32, u32, u32), pred: &mut [i32], tmp: &mut [i32]) {
        let PredBlock { plane, x, y, w, h } = blk;
        let fh = &self.fs.fh;
        let ref_frame = self.t.mi.ref_frame[mi_i][ref_list];
        let cand_mv = self.t.mi.mv[mi_i][ref_list];
        let ref_idx: i32 = if self.b.use_intrabc { -1 } else { fh.ref_frame_idx[ref_frame as usize - LAST_FRAME] as i32 };
        let warp = self.select_warp(blk, ref_frame, ref_idx);
        if warp == WarpKind::None {
            let mv = [cand_mv.row as i32, cand_mv.col as i32];
            let filters = self.t.mi.interp_filter[mi_i];
            let (sx, sy, stx, sty) = self.mv_scaling(plane, ref_idx, x as i32, y as i32, mv);
            let src = McSource { plane, ref_idx, x: sx, y: sy, x_step: stx, y_step: sty };
            self.block_inter_prediction(src, (w, h), filters, rnd, pred, tmp);
        } else {
            let params = if warp == WarpKind::Local { self.b.local_warp_params } else { fh.gm_params[ref_frame as usize] };
            for i8 in 0..=((h - 1) >> 3) {
                for j8 in 0..=((w - 1) >> 3) {
                    self.block_warp(&params, ref_idx, blk, (i8, j8), rnd, pred);
                }
            }
        }
    }

    /// Wedge compound mask (7.11.3.11) for the luma block.
    fn fill_wedge_mask(&mut self, blk: PredBlock) {
        let m = &wedge_masks()[self.b.mi_size][self.b.wedge_index][self.b.wedge_sign];
        for i in 0..blk.h {
            for j in 0..blk.w {
                self.mask[i * PRED_STRIDE + j] = m[i * blk.w + j] as i32;
            }
        }
    }

    /// Inter-intra smooth mask (7.11.3.13).
    fn fill_interintra_mask(&mut self, blk: PredBlock) {
        let size_scale = MAX_SB_SIZE / blk.h.max(blk.w);
        let im = self.b.interintra_mode;
        for i in 0..blk.h {
            for j in 0..blk.w {
                self.mask[i * PRED_STRIDE + j] = if im == II_V_PRED {
                    II_WEIGHTS_1D[i * size_scale] as i32
                } else if im == II_H_PRED {
                    II_WEIGHTS_1D[j * size_scale] as i32
                } else if im == II_SMOOTH_PRED {
                    II_WEIGHTS_1D[i.min(j) * size_scale] as i32
                } else {
                    32
                };
            }
        }
    }

    /// Difference-weight compound mask (7.11.3.12) for the luma block; `diff_shift` rounds the sample difference.
    fn fill_diffwtd_mask(&mut self, blk: PredBlock, preds: &[Vec<i32>; 2], diff_shift: u32) {
        for i in 0..blk.h {
            for j in 0..blk.w {
                let k = i * PRED_STRIDE + j;
                let diff = round2((preds[0][k] - preds[1][k]).abs(), diff_shift);
                let m = (38 + diff / 16).clamp(0, 64);
                self.mask[k] = if self.b.mask_type == 1 { 64 - m } else { m };
            }
        }
    }

    /// Writes the clamped value of `value(k)` (k indexes the prediction buffers) for every sample of `blk`.
    fn store_samples(&mut self, blk: PredBlock, max: i32, value: impl Fn(usize) -> i32) {
        let pl = &mut self.t.cur.planes[blk.plane];
        for i in 0..blk.h {
            let row = &mut pl.row_from_mut(blk.y + i, blk.x)[..blk.w];
            for (j, out) in row.iter_mut().enumerate() {
                *out = value(i * PRED_STRIDE + j).clamp(0, max) as u16;
            }
        }
    }

    /// Combines the reference predictions into the picture according to the compound type (7.11.3.1).
    fn store_prediction(&mut self, blk: PredBlock, preds: &[Vec<i32>; 2], cand_refs: [i8; 2], post_round: u32) {
        let ct = self.b.compound_type;
        let max = (1i32 << self.fs.bit_depth) - 1;
        let is_compound = cand_refs[1] > INTRA_FRAME as i8;
        let is_inter_intra = self.b.is_inter && self.b.ref_frame[1] == INTRA_FRAME as i8;
        if !is_compound && !is_inter_intra {
            self.store_samples(blk, max, |k| preds[0][k]);
        } else if ct == COMPOUND_AVERAGE {
            self.store_samples(blk, max, |k| round2(preds[0][k] + preds[1][k], 1 + post_round));
        } else if ct == COMPOUND_DISTANCE {
            let (fwd, bck) = self.distance_weights(cand_refs);
            self.store_samples(blk, max, |k| round2(fwd * preds[0][k] + bck * preds[1][k], 4 + post_round));
        } else {
            self.mask_blend(preds, blk, post_round, max);
        }
    }

    fn is_scaled_ref(&self, ref_idx: i32) -> bool {
        if ref_idx < 0 {
            return false;
        }
        let fh = &self.fs.fh;
        let ri = &self.fs.ref_info[ref_idx as usize];
        let xs = ((ri.upscaled_width << REF_SCALE_SHIFT) + fh.frame_width / 2) / fh.frame_width;
        let ys = ((ri.frame_height << REF_SCALE_SHIFT) + fh.frame_height / 2) / fh.frame_height;
        xs != 1 << REF_SCALE_SHIFT || ys != 1 << REF_SCALE_SHIFT
    }

    fn mask_blend(&mut self, preds: &[Vec<i32>; 2], blk: PredBlock, post: u32, max: i32) {
        let PredBlock { plane, x: dx, y: dy, w, h } = blk;
        let (sx, sy) = if plane > 0 { (self.fs.ssx, self.fs.ssy) } else { (0, 0) };
        let interintra = self.b.interintra;
        let wedge_ii = self.b.wedge_interintra;
        for yy in 0..h {
            for xx in 0..w {
                let mk = |r: usize, c: usize| self.mask[r * PRED_STRIDE + c];
                let m = if (sx == 0 && sy == 0) || (interintra && !wedge_ii) {
                    mk(yy, xx)
                } else if sx == 1 && sy == 0 {
                    round2(mk(yy, 2 * xx) + mk(yy, 2 * xx + 1), 1)
                } else {
                    round2(mk(2 * yy, 2 * xx) + mk(2 * yy, 2 * xx + 1) + mk(2 * yy + 1, 2 * xx) + mk(2 * yy + 1, 2 * xx + 1), 2)
                };
                let k = yy * PRED_STRIDE + xx;
                let pl = &mut self.t.cur.planes[plane];
                if interintra {
                    let p0 = round2(preds[0][k], post).clamp(0, max);
                    let p1 = pl.at(dx + xx, dy + yy) as i32;
                    pl.set(dx + xx, dy + yy, round2(m * p1 + (64 - m) * p0, 6) as u16);
                } else {
                    let v = round2(m * preds[0][k] + (64 - m) * preds[1][k], 6 + post).clamp(0, max);
                    pl.set(dx + xx, dy + yy, v as u16);
                }
            }
        }
    }

    fn distance_weights(&self, refs: [i8; 2]) -> (i32, i32) {
        let fh = &self.fs.fh;
        let mut dist = [0i32; 2];
        for (rl, d) in dist.iter_mut().enumerate() {
            let h = fh.order_hints[refs[rl] as usize];
            *d = crate::header::relative_dist(&self.fs.seq, h, fh.order_hint).abs().clamp(0, MAX_FRAME_DISTANCE as i32);
        }
        let d0 = dist[1];
        let d1 = dist[0];
        let order = (d0 <= d1) as usize;
        if d0 == 0 || d1 == 0 {
            return (QUANT_DIST_LOOKUP[3][order] as i32, QUANT_DIST_LOOKUP[3][1 - order] as i32);
        }
        let mut i = 0;
        while i < 3 {
            let c0 = QUANT_DIST_WEIGHT[i][order] as i32;
            let c1 = QUANT_DIST_WEIGHT[i][1 - order] as i32;
            if order == 1 {
                if d0 * c0 > d1 * c1 {
                    break;
                }
            } else if d0 * c0 < d1 * c1 {
                break;
            }
            i += 1;
        }
        (QUANT_DIST_LOOKUP[i][order] as i32, QUANT_DIST_LOOKUP[i][1 - order] as i32)
    }

    /// Overlapped motion compensation (7.11.3.9).
    fn overlapped_motion_compensation(&mut self, plane: usize, w: usize, h: usize) {
        let (sx, sy) = if plane > 0 { (self.fs.ssx, self.fs.ssy) } else { (0, 0) };
        let (mi_rows, mi_cols) = (self.fs.fh.mi_rows as usize, self.fs.fh.mi_cols as usize);
        let bd = self.fs.bit_depth;
        let max = (1i32 << bd) - 1;
        let mut obmc = std::mem::take(&mut self.pred_buf[0]);
        let mut tmp = std::mem::take(&mut self.mc_tmp);
        let get_mask = |len: usize| -> &'static [u8] {
            match len {
                2 => &OBMC_MASK_2,
                4 => &OBMC_MASK_4,
                8 => &OBMC_MASK_8,
                16 => &OBMC_MASK_16,
                _ => &OBMC_MASK_32,
            }
        };
        let mut jobs: Vec<(usize, usize, usize, usize, usize, usize, usize)> = Vec::new(); // (pass, cand_row, cand_col, x4, y4, predW, predH)
        if self.b.avail_u && self.fs.plane_residual_size(self.b.mi_size, plane) >= BLOCK_8X8 {
            let w4 = self.b.bw4;
            let mut x4 = self.b.mi_col;
            let y4 = self.b.mi_row;
            let mut n = 0;
            let limit = 4.min(MI_WIDTH_LOG2[self.b.mi_size] as usize);
            while n < limit && x4 < mi_cols.min(self.b.mi_col + w4) {
                let cr = self.b.mi_row - 1;
                let cc = x4 | 1;
                let mi = &self.t.mi;
                let ci = mi.idx(cr, cc);
                let step4 = (NUM_4X4_BLOCKS_WIDE[mi.mi_size[ci] as usize] as usize).clamp(2, 16);
                if mi.ref_frame[ci][0] > INTRA_FRAME as i8 {
                    n += 1;
                    let pw = w.min((step4 * 4) >> sx);
                    let ph = (h >> 1).min(32 >> sy);
                    jobs.push((0, cr, cc, x4, y4, pw, ph));
                }
                x4 += step4;
            }
        }
        if self.b.avail_l {
            let h4 = self.b.bh4;
            let x4 = self.b.mi_col;
            let mut y4 = self.b.mi_row;
            let mut n = 0;
            let limit = 4.min(MI_HEIGHT_LOG2[self.b.mi_size] as usize);
            while n < limit && y4 < mi_rows.min(self.b.mi_row + h4) {
                let cc = self.b.mi_col - 1;
                let cr = y4 | 1;
                let mi = &self.t.mi;
                let ci = mi.idx(cr, cc);
                let step4 = (NUM_4X4_BLOCKS_HIGH[mi.mi_size[ci] as usize] as usize).clamp(2, 16);
                if mi.ref_frame[ci][0] > INTRA_FRAME as i8 {
                    n += 1;
                    let pw = (w >> 1).min(32 >> sx);
                    let ph = h.min((step4 * 4) >> sy);
                    jobs.push((1, cr, cc, x4, y4, pw, ph));
                }
                y4 += step4;
            }
        }
        // the above pass completes before the left pass reads samples; jobs keep that order
        for (pass, cr, cc, x4, y4, pw, ph) in jobs {
            let ci = self.t.mi.idx(cr, cc);
            let m = self.t.mi.mv[ci][0];
            let mv = [m.row as i32, m.col as i32];
            let ref_idx = self.fs.fh.ref_frame_idx[self.t.mi.ref_frame[ci][0] as usize - LAST_FRAME] as i32;
            let px = (x4 * 4) >> sx;
            let py = (y4 * 4) >> sy;
            let (stx, sty, sxs, sys) = self.mv_scaling(plane, ref_idx, px as i32, py as i32, mv);
            let filters = self.t.mi.interp_filter[ci];
            let rnd = rounding(false, bd);
            let src = McSource { plane, ref_idx, x: stx, y: sty, x_step: sxs, y_step: sys };
            self.block_inter_prediction(src, (pw, ph), filters, rnd, &mut obmc, &mut tmp);
            let mask = get_mask(if pass == 0 { ph } else { pw });
            let pl = &mut self.t.cur.planes[plane];
            for i in 0..ph {
                for j in 0..pw {
                    let mm = if pass == 0 { mask[i] } else { mask[j] } as i32;
                    let o = obmc[i * PRED_STRIDE + j].clamp(0, max);
                    let c = pl.at(px + j, py + i) as i32;
                    pl.set(px + j, py + i, round2(mm * c + (64 - mm) * o, 6) as u16);
                }
            }
        }
        self.pred_buf[0] = obmc;
        self.mc_tmp = tmp;
    }
}

const IDENTITY_TAPS: [i16; 8] = [0, 0, 0, 128, 0, 0, 0, 0];

/// Horizontal pass of one row for a block `W` wide: taps outer, columns inner, fixed width so
/// the row stays in registers.
#[inline(always)]
fn hpass<const W: usize>(src: &[u16], hf: &[i16; 8], out: &mut [i32], half: i32, sh: u32) {
    let mut acc = [half; W];
    for t in 0..8 {
        let f = hf[t] as i32;
        let Some(p) = src.get(t..).and_then(|s| s.first_chunk::<W>()) else {
            return;
        };
        for j in 0..W {
            acc[j] += f * p[j] as i32;
        }
    }
    let Some(o) = out.first_chunk_mut::<W>() else {
        return;
    };
    for j in 0..W {
        o[j] = acc[j] >> sh;
    }
}

/// Vertical pass of one output row from 8 intermediate rows (`rows`, stride `W`).
#[inline(always)]
fn vpass<const W: usize>(rows: &[i32], vf: &[i16; 8], out: &mut [i32], half: i32, sh: u32) {
    let mut acc = [half; W];
    for t in 0..8 {
        let f = vf[t] as i32;
        let Some(p) = rows.get(t * W..).and_then(|s| s.first_chunk::<W>()) else {
            return;
        };
        for j in 0..W {
            acc[j] += f * p[j];
        }
    }
    let Some(o) = out.first_chunk_mut::<W>() else {
        return;
    };
    for j in 0..W {
        o[j] = acc[j] >> sh;
    }
}

/// Block inter prediction without reference scaling: every column / row uses the same filter
/// phase, so the filters run over whole rows (bit-identical to the per-sample process).
fn mc_unscaled(
    refp: &Plane,
    (last_x, last_y): (i32, i32),
    blk: McBlock,
    (hf, vf): (&[i16; 8], &[i16; 8]),
    rnd: (u32, u32, u32),
    pred: &mut [i32],
    tmp: &mut [i32],
) {
    let McBlock { x, y, w, h } = blk;
    let x0 = (x >> 10) - 3;
    let y0 = (y >> 10) - 3;
    let h_copy = *hf == IDENTITY_TAPS;
    let v_copy = *vf == IDENTITY_TAPS;
    // With the identity vertical phase only intermediate rows 3 .. 3 + h have a non-zero tap.
    let (r_lo, r_hi) = if v_copy { (3, 3 + h) } else { (0, h + 7) };
    let inside = x0 >= 0 && x0 + w as i32 + 7 <= last_x + 1;
    let mut edge = [0u16; 128 + 8];
    let mut acc = [0i32; 128];
    let (r0, r1) = (rnd.0, rnd.1);
    let (half0, half1) = (1i32 << (r0 - 1), 1i32 << (r1 - 1));
    for r in r_lo..r_hi {
        let ry = (y0 + r as i32).clamp(0, last_y) as usize;
        let row = refp.row(ry);
        let src: &[u16] = if inside {
            &row[x0 as usize..x0 as usize + w + 7]
        } else {
            for (k, e) in edge[..w + 7].iter_mut().enumerate() {
                *e = row[(x0 + k as i32).clamp(0, last_x) as usize];
            }
            &edge[..w + 7]
        };
        let out = &mut tmp[r * w..r * w + w];
        if h_copy {
            // Round2(128 * v, InterRound0) is exact.
            for (o, &v) in out.iter_mut().zip(&src[3..3 + w]) {
                *o = (v as i32) << (7 - r0);
            }
        } else {
            match w {
                2 => hpass::<2>(src, hf, out, half0, r0),
                4 => hpass::<4>(src, hf, out, half0, r0),
                8 => hpass::<8>(src, hf, out, half0, r0),
                16 => hpass::<16>(src, hf, out, half0, r0),
                32 => hpass::<32>(src, hf, out, half0, r0),
                64 => hpass::<64>(src, hf, out, half0, r0),
                _ => {
                    let acc = &mut acc[..w];
                    acc.fill(0);
                    for t in 0..8 {
                        let f = hf[t] as i32;
                        for (a, &v) in acc.iter_mut().zip(&src[t..t + w]) {
                            *a += f * v as i32;
                        }
                    }
                    for (o, &a) in out.iter_mut().zip(acc.iter()) {
                        *o = (a + half0) >> r0;
                    }
                }
            }
        }
    }
    for r in 0..h {
        let out = &mut pred[r * PRED_STRIDE..r * PRED_STRIDE + w];
        if v_copy {
            for (o, &v) in out.iter_mut().zip(&tmp[(r + 3) * w..(r + 4) * w]) {
                *o = (128 * v + half1) >> r1;
            }
        } else {
            let rows = &tmp[r * w..(r + 8) * w];
            match w {
                2 => vpass::<2>(rows, vf, out, half1, r1),
                4 => vpass::<4>(rows, vf, out, half1, r1),
                8 => vpass::<8>(rows, vf, out, half1, r1),
                16 => vpass::<16>(rows, vf, out, half1, r1),
                32 => vpass::<32>(rows, vf, out, half1, r1),
                64 => vpass::<64>(rows, vf, out, half1, r1),
                _ => {
                    let acc = &mut acc[..w];
                    acc.fill(0);
                    for t in 0..8 {
                        let f = vf[t] as i32;
                        for (a, &v) in acc.iter_mut().zip(&tmp[(r + t) * w..(r + t + 1) * w]) {
                            *a += f * v;
                        }
                    }
                    for (o, &a) in out.iter_mut().zip(acc.iter()) {
                        *o = (a + half1) >> r1;
                    }
                }
            }
        }
    }
}
