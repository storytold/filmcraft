//! Super-resolution upscaling (7.16) and loop restoration (7.17).

use crate::frame::{FrameBuf, Plane, RowBand};
use crate::par::Pool;
use crate::postfilter::bands;
use crate::spec_tables::{
    FILTER_BITS, RESTORE_NONE, RESTORE_SGRPROJ, RESTORE_WIENER, SGR_PARAMS, SGRPROJ_MTABLE_BITS, SGRPROJ_PRJ_BITS, SGRPROJ_RECIP_BITS, SGRPROJ_RST_BITS,
    SGRPROJ_SGR_BITS, SUPERRES_EXTRA_BITS, SUPERRES_FILTER_OFFSET, SUPERRES_FILTER_TAPS, SUPERRES_SCALE_BITS, SUPERRES_SCALE_MASK, UPSCALE_FILTER,
};
use crate::state::FrameState;

/// Upscale a frame horizontally from FrameWidth to UpscaledWidth (7.16).
pub(crate) fn upscale(fs: &FrameState, input: &FrameBuf) -> FrameBuf {
    let fh = &fs.fh;
    let up_w = fh.upscaled_width as usize;
    let mut out = FrameBuf::new(up_w, fh.frame_height as usize, fs.num_planes, fs.ssx, fs.ssy, fs.bit_depth as u8);
    let max = (1i32 << fs.bit_depth) - 1;
    for plane in 0..fs.num_planes {
        let (sub_x, sub_y) = if plane > 0 { (fs.ssx, fs.ssy) } else { (0, 0) };
        let down_w = ((fh.frame_width as usize) + sub_x) >> sub_x;
        let up_pw = (up_w + sub_x) >> sub_x;
        let plane_h = ((fh.frame_height as usize) + sub_y) >> sub_y;
        let sb = SUPERRES_SCALE_BITS as i64;
        let step_x = (((down_w as i64) << sb) + (up_pw as i64 / 2)) / up_pw as i64;
        let err = up_pw as i64 * step_x - ((down_w as i64) << sb);
        let mut initial = (-(((up_pw - down_w) as i64) << (sb - 1)) + up_pw as i64 / 2) / up_pw as i64 + (1 << (SUPERRES_EXTRA_BITS - 1)) - err / 2;
        initial &= SUPERRES_SCALE_MASK as i64;
        let mi_w = fh.mi_cols as usize >> sub_x;
        let max_x = (mi_w * 4 - 1) as i64;
        let src = &input.planes[plane];
        let dst = &mut out.planes[plane];
        for y in 0..plane_h {
            let row = src.row(y);
            for x in 0..up_pw {
                let src_x = -(1i64 << sb) + initial + x as i64 * step_x;
                let src_px = src_x >> sb;
                let subpel = ((src_x & SUPERRES_SCALE_MASK as i64) >> SUPERRES_EXTRA_BITS) as usize;
                let mut sum = 0i32;
                for k in 0..SUPERRES_FILTER_TAPS {
                    let sx = (src_px + k as i64 - SUPERRES_FILTER_OFFSET as i64).clamp(0, max_x) as usize;
                    sum += row[sx] as i32 * UPSCALE_FILTER[subpel][k] as i32;
                }
                dst.set(x, y, ((sum + 64) >> FILTER_BITS).clamp(0, max) as u16);
            }
        }
    }
    out
}

struct LrCtx<'a> {
    cur: &'a Plane,
    cdef: &'a Plane,
    plane_end_x: i32,
    plane_end_y: i32,
    stripe_start_y: i32,
    stripe_end_y: i32,
}

impl LrCtx<'_> {
    /// Get source sample process (7.17.6).
    #[inline(always)]
    fn sample(&self, x: i32, y: i32) -> i32 {
        let x = x.min(self.plane_end_x).max(0) as usize;
        let y = y.min(self.plane_end_y).max(0);
        if y < self.stripe_start_y {
            let y = (self.stripe_start_y - 2).max(y) as usize;
            self.cur.at(x, y) as i32
        } else if y > self.stripe_end_y {
            let y = (self.stripe_end_y + 2).min(y) as usize;
            self.cur.at(x, y) as i32
        } else {
            self.cdef.at(x, y as usize) as i32
        }
    }
}

/// Loop restoration (7.17): returns LrFrame given UpscaledCurrFrame and UpscaledCdefFrame.
/// Bands of 64 luma rows are restored in parallel: every unit reads only the two input frames
/// and writes only its own samples.
pub(crate) fn loop_restoration(fs: &FrameState, cur: &FrameBuf, cdef: &FrameBuf, pool: &Pool) -> FrameBuf {
    let fh = &fs.fh;
    let mut lr = cdef.clone();
    let up_w = fh.upscaled_width as i32;
    let fr_h = fh.frame_height as i32;
    let bd = fs.bit_depth;
    let ranges = bands(pool, fh.frame_height as usize);
    let mut planes = lr.row_bands(&ranges);
    pool.map_mut(&mut planes, |b, band| {
        // Rows of 4 luma samples in this band.
        let (start, end) = ranges[b];
        let (row0, row1) = ((start / 4) as i32, (end / 4).min(fh.frame_height as usize) as i32);
        for (plane, out) in band.iter_mut().enumerate().take(fs.num_planes) {
            let rtype_frame = fh.lr.frame_restoration_type[plane];
            if rtype_frame == RESTORE_NONE as u8 {
                continue;
            }
            let (sub_x, sub_y) = if plane > 0 { (fs.ssx as i32, fs.ssy as i32) } else { (0, 0) };
            let unit_size = fh.lr.loop_restoration_size[plane] as i32;
            let unit_rows = fs.lr_unit_rows[plane] as i32;
            let unit_cols = fs.lr_unit_cols[plane] as i32;
            let plane_end_x = ((up_w + sub_x) >> sub_x) - 1;
            let plane_end_y = ((fr_h + sub_y) >> sub_y) - 1;
            let mut row = row0;
            while row < row1 && row * 4 < fr_h {
                let luma_y = row * 4;
                let stripe_num = (luma_y + 8) / 64;
                let stripe_start_y = (-8 + stripe_num * 64) >> sub_y;
                let stripe_end_y = stripe_start_y + (64 >> sub_y) - 1;
                let ctx = LrCtx { cur: &cur.planes[plane], cdef: &cdef.planes[plane], plane_end_x, plane_end_y, stripe_start_y, stripe_end_y };
                let unit_row = (unit_rows - 1).min(((row * 4 + 8) >> sub_y) / unit_size);
                let mut col = 0;
                while col * 4 < up_w {
                    let unit_col = (unit_cols - 1).min(((col * 4) >> sub_x) / unit_size);
                    let x = (col * 4) >> sub_x;
                    let y = (row * 4) >> sub_y;
                    let w = (4 >> sub_x).min(plane_end_x - x + 1);
                    let h = (4 >> sub_y).min(plane_end_y - y + 1);
                    if w > 0 && h > 0 {
                        let ui = (unit_row * unit_cols + unit_col) as usize;
                        let blk = LrBlock { x, y, w, h, bd };
                        match fs.lr.lr_type[plane][ui] {
                            t if t == RESTORE_WIENER as u8 => wiener(fs, &ctx, out, plane, ui, blk),
                            t if t == RESTORE_SGRPROJ as u8 => self_guided(fs, &ctx, out, plane, ui, blk),
                            _ => {}
                        }
                    }
                    col += 1;
                }
                row += 1;
            }
        }
    });
    drop(planes);
    lr
}

/// A (up to 4x4) block of plane samples to restore, with the bit depth.
#[derive(Clone, Copy)]
struct LrBlock {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    bd: u32,
}

fn wiener(fs: &FrameState, ctx: &LrCtx, out: &mut RowBand, plane: usize, ui: usize, blk: LrBlock) {
    let LrBlock { x, y, w, h, bd } = blk;
    let round0: u32 = if bd == 12 { 5 } else { 3 };
    let round1: u32 = if bd == 12 { 9 } else { 11 };
    let coef = &fs.lr.lr_wiener[plane][ui];
    let mk = |c: &[i8; 3]| -> [i32; 7] {
        let mut f = [0i32; 7];
        f[3] = 128;
        for i in 0..3 {
            f[i] = c[i] as i32;
            f[6 - i] = c[i] as i32;
            f[3] -= 2 * c[i] as i32;
        }
        f
    };
    let vfilter = mk(&coef[0]);
    let hfilter = mk(&coef[1]);
    let fb = FILTER_BITS as u32;
    let offset = 1i32 << (bd + fb - round0 - 1);
    let limit = (1i32 << (bd + 1 + fb - round0)) - 1;
    let mut inter = [[0i32; 4]; 10];
    for r in 0..(h + 6) as usize {
        for c in 0..w as usize {
            let mut s = 0;
            for t in 0..7 {
                s += hfilter[t] * ctx.sample(x + c as i32 + t as i32 - 3, y + r as i32 - 3);
            }
            let v = (s + (1 << (round0 - 1))) >> round0;
            inter[r][c] = v.clamp(-offset, limit - offset);
        }
    }
    let max = (1i32 << bd) - 1;
    for r in 0..h as usize {
        for c in 0..w as usize {
            let mut s = 0;
            for t in 0..7 {
                s += vfilter[t] * inter[r + t][c];
            }
            let v = (s + (1 << (round1 - 1))) >> round1;
            out.set(x as usize + c, y as usize + r, v.clamp(0, max) as u16);
        }
    }
}

fn self_guided(fs: &FrameState, ctx: &LrCtx, out: &mut RowBand, plane: usize, ui: usize, blk: LrBlock) {
    let LrBlock { x, y, w, h, bd } = blk;
    let set = fs.lr.lr_sgr_set[plane][ui] as usize;
    let mut flt0 = [[0i32; 4]; 4];
    let mut flt1 = [[0i32; 4]; 4];
    box_filter(ctx, blk, set, 0, &mut flt0);
    box_filter(ctx, blk, set, 1, &mut flt1);
    let xqd = fs.lr.lr_sgr_xqd[plane][ui];
    let w0 = xqd[0] as i32;
    let w1 = xqd[1] as i32;
    let w2 = (1 << SGRPROJ_PRJ_BITS) - w0 - w1;
    let r0 = SGR_PARAMS[set][0];
    let r1 = SGR_PARAMS[set][2];
    let max = (1i32 << bd) - 1;
    let sh = (SGRPROJ_RST_BITS + SGRPROJ_PRJ_BITS) as u32;
    for i in 0..h as usize {
        for j in 0..w as usize {
            let u = (ctx.cdef.at(x as usize + j, y as usize + i) as i32) << SGRPROJ_RST_BITS;
            let mut v = w1 * u;
            v += if r0 != 0 { w0 * flt0[i][j] } else { w0 * u };
            v += if r1 != 0 { w2 * flt1[i][j] } else { w2 * u };
            let s = (v + (1 << (sh - 1))) >> sh;
            out.set(x as usize + j, y as usize + i, s.clamp(0, max) as u16);
        }
    }
}

#[inline(always)]
fn round2_64(x: i64, n: u32) -> i64 {
    if n == 0 { x } else { (x + (1i64 << (n - 1))) >> n }
}

fn box_filter(ctx: &LrCtx, blk: LrBlock, set: usize, pass: usize, f: &mut [[i32; 4]; 4]) {
    let LrBlock { x, y, w, h, bd } = blk;
    let r = SGR_PARAMS[set][pass * 2] as i32;
    if r == 0 {
        return;
    }
    let eps = SGR_PARAMS[set][pass * 2 + 1] as i64;
    let n = ((2 * r + 1) * (2 * r + 1)) as i64;
    let n2e = n * n * eps;
    let s = ((1i64 << SGRPROJ_MTABLE_BITS) + n2e / 2) / n2e;
    let one_over_n = ((1i64 << SGRPROJ_RECIP_BITS) + n / 2) / n;
    let mut a_arr = [[0i64; 6]; 6];
    let mut b_arr = [[0i64; 6]; 6];
    for i in -1..h + 1 {
        for j in -1..w + 1 {
            let mut a = 0i64;
            let mut b = 0i64;
            for dy in -r..=r {
                for dx in -r..=r {
                    let c = ctx.sample(x + j + dx, y + i + dy) as i64;
                    a += c * c;
                    b += c;
                }
            }
            let a = round2_64(a, 2 * (bd - 8));
            let d = round2_64(b, bd - 8);
            let p = (a * n - d * d).max(0);
            let z = round2_64(p * s, SGRPROJ_MTABLE_BITS as u32);
            let a2 = if z >= 255 {
                256
            } else if z == 0 {
                1
            } else {
                ((z << SGRPROJ_SGR_BITS) + z / 2) / (z + 1)
            };
            let b2 = ((1i64 << SGRPROJ_SGR_BITS) - a2) * b * one_over_n;
            a_arr[(i + 1) as usize][(j + 1) as usize] = a2;
            b_arr[(i + 1) as usize][(j + 1) as usize] = round2_64(b2, SGRPROJ_RECIP_BITS as u32);
        }
    }
    for i in 0..h {
        let shift = if pass == 0 && (i & 1) == 1 { 4 } else { 5 };
        for j in 0..w {
            let mut a = 0i64;
            let mut b = 0i64;
            for dy in -1..=1i32 {
                for dx in -1..=1i32 {
                    let weight = if pass == 0 {
                        if ((i + dy) & 1) != 0 { if dx == 0 { 6 } else { 5 } } else { 0 }
                    } else if dx == 0 || dy == 0 {
                        4
                    } else {
                        3
                    };
                    a += weight * a_arr[(i + dy + 1) as usize][(j + dx + 1) as usize];
                    b += weight * b_arr[(i + dy + 1) as usize][(j + dx + 1) as usize];
                }
            }
            let v = a * ctx.cdef.at((x + j) as usize, (y + i) as usize) as i64 + b;
            f[i as usize][j as usize] = round2_64(v, (SGRPROJ_SGR_BITS + shift - SGRPROJ_RST_BITS) as u32) as i32;
        }
    }
}
