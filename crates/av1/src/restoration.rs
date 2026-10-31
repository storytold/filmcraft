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

/// The rows of the deblocked (and upscaled) frame next to the stripe boundaries of a plane,
/// which loop restoration reads above and below each stripe (7.17.6) once CDEF has replaced the
/// frame: rows B - 2 .. B + 2 of every boundary B = first + k * period.
#[derive(Default)]
pub(crate) struct StripeEdges {
    data: Vec<u16>,
    stride: usize,
    first: i32,
    period: i32,
}

impl StripeEdges {
    /// The edge rows of `plane` (clamped to its stored rows), for boundaries up to row `last`.
    pub(crate) fn save(plane: &Plane, first: i32, period: i32, last: i32) -> StripeEdges {
        let mut data = Vec::new();
        let rows = plane.rows as i32;
        let mut b = first;
        while rows > 0 && period > 0 && b - 2 <= last {
            for y in b - 2..b + 2 {
                data.extend_from_slice(plane.row(y.clamp(0, rows - 1) as usize));
            }
            b += period;
        }
        StripeEdges { data, stride: plane.stride, first, period }
    }

    /// The saved row `y` (one of B - 2 .. B + 2 of a boundary B).
    #[inline(always)]
    fn row(&self, y: i32) -> &[u16] {
        let k = (y - self.first + 2).div_euclid(self.period.max(1));
        let i = (k * 4 + y + 2 - (self.first + k * self.period)).max(0) as usize;
        self.data.get(i * self.stride..(i + 1) * self.stride).unwrap_or(&[])
    }
}

/// The edge rows of every plane of `cur` that loop restoration filters (stripes of 64 luma rows,
/// the first ending 8 rows early).
pub(crate) fn save_stripe_edges(fs: &FrameState, cur: &FrameBuf) -> [StripeEdges; 3] {
    std::array::from_fn(|p| {
        if p >= fs.num_planes || fs.fh.lr.frame_restoration_type[p] == RESTORE_NONE as u8 {
            return StripeEdges::default();
        }
        let sub_y = if p == 0 { 0 } else { fs.ssy as i32 };
        let last = ((fs.fh.frame_height as i32 + sub_y) >> sub_y) - 1;
        StripeEdges::save(&cur.planes[p], 56 >> sub_y, 64 >> sub_y, last)
    })
}

/// Rows of a plane band, addressed with the plane's coordinates.
#[derive(Clone, Copy)]
struct Rows<'a> {
    data: &'a [u16],
    stride: usize,
    y0: usize,
}

impl<'a> Rows<'a> {
    fn of(b: &'a RowBand) -> Rows<'a> {
        Rows { data: b.data, stride: b.stride, y0: b.y0 }
    }
    #[inline(always)]
    fn row(&self, y: usize) -> &'a [u16] {
        let o = y.wrapping_sub(self.y0).wrapping_mul(self.stride);
        self.data.get(o..o.wrapping_add(self.stride)).unwrap_or(&[])
    }
}

struct LrCtx<'a> {
    edges: &'a StripeEdges,
    cdef: Rows<'a>,
    plane_end_x: i32,
    plane_end_y: i32,
    stripe_start_y: i32,
    stripe_end_y: i32,
}

impl LrCtx<'_> {
    /// The row the get source sample process (7.17.6) reads for row `y`: CDEF output inside
    /// the stripe, the deblocked frame for the (at most 2) rows above and below it.
    #[inline(always)]
    fn source_row(&self, y: i32) -> &[u16] {
        let y = y.min(self.plane_end_y).max(0);
        if y < self.stripe_start_y {
            self.edges.row((self.stripe_start_y - 2).max(y))
        } else if y > self.stripe_end_y {
            self.edges.row((self.stripe_end_y + 2).min(y))
        } else {
            self.cdef.row(y as usize)
        }
    }

    /// The source samples (7.17.6) of the rectangle (x, y, w, h) and 3 more on every side:
    /// `src[r * (w + 6) + c]` is the sample at (x + c - 3, y + r - 3).
    fn fetch(&self, x: i32, y: i32, w: usize, h: usize, src: &mut Vec<u16>) {
        src.clear();
        for r in 0..h as i32 + 6 {
            let row = self.source_row(y + r - 3);
            src.extend((x - 3..x + w as i32 + 3).map(|c| row.get(c.min(self.plane_end_x).max(0) as usize).copied().unwrap_or(0)));
        }
    }
}

/// Buffers of one band's restoration, kept between rectangles.
#[derive(Default)]
struct LrScratch {
    src: Vec<u16>,
    /// The next rectangle's source, fetched before this one is written.
    next: Vec<u16>,
    inter: Vec<i32>,
    sum: Vec<i32>,
    sq: Vec<i32>,
    a: Vec<i32>,
    b: Vec<i32>,
    flt: [Vec<i32>; 2],
}

/// A rectangle to restore: its position, unit and filter.
#[derive(Clone, Copy)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    unit: usize,
    sgr: bool,
}

/// Loop restoration (7.17) in place: `frame` holds UpscaledCdefFrame and becomes LrFrame;
/// `edges` are the rows of UpscaledCurrFrame next to the stripe boundaries. Each band is whole
/// stripes, restored in parallel: a stripe reads only its own rows and the saved edges.
///
/// The spec filters 4x4 blocks; here each rectangle of neighbouring blocks that share a stripe
/// and a restoration unit is filtered at once (the same arithmetic for every sample, without
/// redoing the rows and columns that neighbouring blocks share). A stripe lies in one row of
/// units (unit sizes are multiples of the stripe height), and its rectangles run left to right,
/// each fetched before the one on its left is written.
pub(crate) fn loop_restoration(fs: &FrameState, frame: &mut FrameBuf, edges: &[StripeEdges; 3], pool: &Pool) {
    let fh = &fs.fh;
    let up_w = fh.upscaled_width as i32;
    let fr_h = fh.frame_height as i32;
    let bd = fs.bit_depth;
    let ranges: Vec<(usize, usize)> = bands(pool, fh.frame_height as usize).into_iter().map(|(s, e)| (s.saturating_sub(8), e.saturating_sub(8))).collect();
    let mut planes = frame.row_bands(&ranges);
    pool.map_mut(&mut planes, |b, band| {
        let mut scratch = LrScratch::default();
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
            let stripe = |row: i32| (row * 4 + 8) / 64;
            let unit_row = |row: i32| (unit_rows - 1).min(((row * 4 + 8) >> sub_y) / unit_size);
            let unit_col = |col: i32| (unit_cols - 1).min(((col * 4) >> sub_x) / unit_size);
            let rows_end = row1.min((fr_h + 3) / 4);
            let cols_end = (up_w + 3) / 4;
            let mut row = row0;
            while row < rows_end {
                // Rows of blocks in the same stripe and unit row.
                let mut next = row + 1;
                while next < rows_end && stripe(next) == stripe(row) && unit_row(next) == unit_row(row) {
                    next += 1;
                }
                let stripe_start_y = (-8 + stripe(row) * 64) >> sub_y;
                let stripe_end_y = stripe_start_y + (64 >> sub_y) - 1;
                let y = (row * 4) >> sub_y;
                let h = ((next * 4) >> sub_y).min(plane_end_y + 1) - y;
                let mut pending: Option<Rect> = None;
                let mut col = 0;
                while col <= cols_end {
                    let mut rect = None;
                    let mut next_col = col + 1;
                    if col < cols_end {
                        while next_col < cols_end && unit_col(next_col) == unit_col(col) {
                            next_col += 1;
                        }
                        let x = (col * 4) >> sub_x;
                        let w = ((next_col * 4) >> sub_x).min(plane_end_x + 1) - x;
                        let unit = (unit_row(row) * unit_cols + unit_col(col)) as usize;
                        let t = fs.lr.lr_type[plane].get(unit).copied().unwrap_or(RESTORE_NONE as u8);
                        if w > 0 && h > 0 && (t == RESTORE_WIENER as u8 || t == RESTORE_SGRPROJ as u8) {
                            let r = Rect { x: x as usize, y: y as usize, w: w as usize, h: h as usize, unit, sgr: t == RESTORE_SGRPROJ as u8 };
                            let ctx = LrCtx { edges: &edges[plane], cdef: Rows::of(out), plane_end_x, plane_end_y, stripe_start_y, stripe_end_y };
                            ctx.fetch(x, y, r.w, r.h, &mut scratch.next);
                            rect = Some(r);
                        }
                    }
                    // The rectangle on the left, now that its right neighbour has its source.
                    if rect.is_some() || col == cols_end {
                        if let Some(p) = pending.take() {
                            restore(fs, plane, &p, &mut scratch, out, bd);
                        }
                        if rect.is_some() {
                            std::mem::swap(&mut scratch.src, &mut scratch.next);
                            pending = rect;
                        }
                    }
                    col = next_col;
                }
                row = next;
            }
        }
    });
}

/// Filter the rectangle `r` of `plane` whose source is in `s.src`.
fn restore(fs: &FrameState, plane: usize, r: &Rect, s: &mut LrScratch, out: &mut RowBand, bd: u32) {
    if r.sgr {
        let (set, xqd) = (fs.lr.lr_sgr_set[plane][r.unit] as usize, fs.lr.lr_sgr_xqd[plane][r.unit]);
        self_guided(set, xqd, s, out, r, bd);
    } else {
        wiener(&fs.lr.lr_wiener[plane][r.unit], s, out, r, bd);
    }
}

/// The Wiener filter taps (7.17.4) from the three coded coefficients of one direction.
fn wiener_taps(c: &[i8; 3]) -> [i32; 7] {
    let mut f = [0i32; 7];
    f[3] = 128;
    for i in 0..3 {
        f[i] = c[i] as i32;
        f[6 - i] = c[i] as i32;
        f[3] -= 2 * c[i] as i32;
    }
    f
}

/// Wiener filter process (7.17.4) of the rectangle (x, y, w, h) whose source samples are in
/// `s.src`: the horizontal pass over its rows and 3 more above and below, then the vertical.
fn wiener(coef: &[[i8; 3]; 2], s: &mut LrScratch, out: &mut RowBand, r: &Rect, bd: u32) {
    let Rect { x, y, w, h, .. } = *r;
    let round0: u32 = if bd == 12 { 5 } else { 3 };
    let round1: u32 = if bd == 12 { 9 } else { 11 };
    let vfilter = wiener_taps(&coef[0]);
    let hfilter = wiener_taps(&coef[1]);
    let fb = FILTER_BITS as u32;
    let offset = 1i32 << (bd + fb - round0 - 1);
    let limit = (1i32 << (bd + 1 + fb - round0)) - 1;
    let sw = w + 6;
    s.inter.clear();
    for r in 0..h + 6 {
        let src = &s.src[r * sw..(r + 1) * sw];
        s.inter.extend(src.windows(7).take(w).map(|t| {
            let v = t.iter().zip(&hfilter).map(|(&p, &f)| f * p as i32).sum::<i32>();
            ((v + (1 << (round0 - 1))) >> round0).clamp(-offset, limit - offset)
        }));
    }
    let max = (1i32 << bd) - 1;
    for r in 0..h {
        let row = &mut out.row_mut(y + r)[x..x + w];
        for (c, o) in row.iter_mut().enumerate() {
            let v = (0..7).map(|t| vfilter[t] * s.inter[(r + t) * w + c]).sum::<i32>();
            *o = ((v + (1 << (round1 - 1))) >> round1).clamp(0, max) as u16;
        }
    }
}

/// Self guided filter process (7.17.3) of the rectangle (x, y, w, h) whose source samples are
/// in `s.src`.
fn self_guided(set: usize, xqd: [i8; 2], s: &mut LrScratch, out: &mut RowBand, r: &Rect, bd: u32) {
    let Rect { x, y, w, h, .. } = *r;
    for pass in 0..2 {
        box_filter(s, w, h, set, pass, bd);
    }
    let w0 = xqd[0] as i32;
    let w1 = xqd[1] as i32;
    let w2 = (1 << SGRPROJ_PRJ_BITS) - w0 - w1;
    let r0 = SGR_PARAMS[set][0];
    let r1 = SGR_PARAMS[set][2];
    let max = (1i32 << bd) - 1;
    let sh = (SGRPROJ_RST_BITS + SGRPROJ_PRJ_BITS) as u32;
    let sw = w + 6;
    for i in 0..h {
        let cdef = &s.src[(i + 3) * sw + 3..(i + 3) * sw + 3 + w];
        let row = &mut out.row_mut(y + i)[x..x + w];
        for (j, (o, &c)) in row.iter_mut().zip(cdef).enumerate() {
            let u = (c as i32) << SGRPROJ_RST_BITS;
            let mut v = w1 * u;
            v += if r0 != 0 { w0 * s.flt[0][i * w + j] } else { w0 * u };
            v += if r1 != 0 { w2 * s.flt[1][i * w + j] } else { w2 * u };
            *o = ((v + (1 << (sh - 1))) >> sh).clamp(0, max) as u16;
        }
    }
}

#[inline(always)]
fn round2_64(x: i64, n: u32) -> i64 {
    if n == 0 { x } else { (x + (1i64 << (n - 1))) >> n }
}

/// a2 of the box filter (7.17.3) for each z: 256 from 255 up, otherwise
/// ((z << SGRPROJ_SGR_BITS) + z / 2) / (z + 1), and 1 for 0.
const SGR_A2: [i32; 256] = {
    let mut t = [256i32; 256];
    let mut z = 0;
    while z < 255 {
        t[z] = if z == 0 { 1 } else { (((z << SGRPROJ_SGR_BITS) + z / 2) / (z + 1)) as i32 };
        z += 1;
    }
    t
};

/// Box filter process (7.17.3) for `pass` of a w x h rectangle whose source samples are in
/// `s.src`: `s.flt[pass]`, w per row. A and B are needed one position around the rectangle; the box sums over each
/// position's (2r + 1)^2 neighbourhood are taken down the columns, then along the rows. The
/// first pass reads A and B only on every other row (from the row above the rectangle on).
fn box_filter(s: &mut LrScratch, w: usize, h: usize, set: usize, pass: usize, bd: u32) {
    let r = SGR_PARAMS[set][pass * 2] as usize;
    if r == 0 {
        return;
    }
    let eps = SGR_PARAMS[set][pass * 2 + 1] as i64;
    let n = ((2 * r + 1) * (2 * r + 1)) as i64;
    let n2e = n * n * eps;
    let sc = ((1i64 << SGRPROJ_MTABLE_BITS) + n2e / 2) / n2e;
    let one_over_n = ((1i64 << SGRPROJ_RECIP_BITS) + n / 2) / n;
    let (sw, gw) = (w + 6, w + 2);
    s.a.clear();
    s.a.resize((h + 2) * gw, 0);
    s.b.clear();
    s.b.resize((h + 2) * gw, 0);
    // Grid row g is position i = g - 1, centred on source row g + 2.
    for g in (0..h + 2).step_by(if pass == 0 { 2 } else { 1 }) {
        s.sum.clear();
        s.sq.clear();
        for c in 0..sw {
            let (mut b, mut a) = (0i32, 0i32);
            for k in g + 2 - r..=g + 2 + r {
                let v = s.src[k * sw + c] as i32;
                b += v;
                a += v * v;
            }
            s.sum.push(b);
            s.sq.push(a);
        }
        for j in 0..gw {
            // Position j - 1, centred on source column j + 2.
            let b = s.sum[j + 2 - r..=j + 2 + r].iter().sum::<i32>() as i64;
            let a = s.sq[j + 2 - r..=j + 2 + r].iter().sum::<i32>() as i64;
            let a = round2_64(a, 2 * (bd - 8));
            let d = round2_64(b, bd - 8);
            let p = (a * n - d * d).max(0);
            let z = round2_64(p * sc, SGRPROJ_MTABLE_BITS as u32);
            let a2 = SGR_A2[z.clamp(0, 255) as usize] as i64;
            let b2 = ((1i64 << SGRPROJ_SGR_BITS) - a2) * b * one_over_n;
            s.a[g * gw + j] = a2 as i32;
            s.b[g * gw + j] = round2_64(b2, SGRPROJ_RECIP_BITS as u32) as i32;
        }
    }
    let f = &mut s.flt[pass];
    f.clear();
    for i in 0..h {
        let shift = if pass == 0 && (i & 1) == 1 { 4 } else { 5 };
        for j in 0..w {
            let c = s.src[(i + 3) * sw + 3 + j];
            let (mut a, mut b) = (0i32, 0i32);
            for dy in 0..3 {
                for dx in 0..3 {
                    let weight = if pass == 0 {
                        if (i + dy) & 1 == 0 { if dx == 1 { 6 } else { 5 } } else { 0 }
                    } else if dx == 1 || dy == 1 {
                        4
                    } else {
                        3
                    };
                    a += weight * s.a[(i + dy) * gw + j + dx];
                    b += weight * s.b[(i + dy) * gw + j + dx];
                }
            }
            let v = a as i64 * c as i64 + b as i64;
            f.push(round2_64(v, (SGRPROJ_SGR_BITS + shift - SGRPROJ_RST_BITS) as u32) as i32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl LrCtx<'_> {
        /// Get source sample process (7.17.6), one sample at a time.
        fn sample(&self, x: i32, y: i32) -> i32 {
            self.source_row(y)[x.min(self.plane_end_x).max(0) as usize] as i32
        }
    }

    /// 7.17.4 for one block, as the spec writes it.
    fn spec_wiener(coef: &[[i8; 3]; 2], ctx: &LrCtx, out: &mut Plane, (x, y, w, h): (i32, i32, i32, i32), bd: u32) {
        let round0: u32 = if bd == 12 { 5 } else { 3 };
        let round1: u32 = if bd == 12 { 9 } else { 11 };
        let (vfilter, hfilter) = (wiener_taps(&coef[0]), wiener_taps(&coef[1]));
        let fb = FILTER_BITS as u32;
        let offset = 1i32 << (bd + fb - round0 - 1);
        let limit = (1i32 << (bd + 1 + fb - round0)) - 1;
        let mut inter = [[0i32; 4]; 10];
        for r in 0..(h + 6) as usize {
            for c in 0..w as usize {
                let s: i32 = (0..7).map(|t| hfilter[t] * ctx.sample(x + c as i32 + t as i32 - 3, y + r as i32 - 3)).sum();
                inter[r][c] = ((s + (1 << (round0 - 1))) >> round0).clamp(-offset, limit - offset);
            }
        }
        for r in 0..h as usize {
            for c in 0..w as usize {
                let s: i32 = (0..7).map(|t| vfilter[t] * inter[r + t][c]).sum();
                out.set(x as usize + c, y as usize + r, ((s + (1 << (round1 - 1))) >> round1).clamp(0, (1 << bd) - 1) as u16);
            }
        }
    }

    /// 7.17.3 for one block, as the spec writes it.
    fn spec_self_guided(set: usize, xqd: [i8; 2], ctx: &LrCtx, out: &mut Plane, (x, y, w, h): (i32, i32, i32, i32), bd: u32) {
        let mut flt = [[[0i32; 4]; 4]; 2];
        for (pass, f) in flt.iter_mut().enumerate() {
            let r = SGR_PARAMS[set][pass * 2] as i32;
            if r == 0 {
                continue;
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
                    let (mut a, mut b) = (0i64, 0i64);
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
                    let (mut a, mut b) = (0i64, 0i64);
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
                    let v = a * ctx.cdef.row((y + i) as usize)[(x + j) as usize] as i64 + b;
                    f[i as usize][j as usize] = round2_64(v, (SGRPROJ_SGR_BITS + shift - SGRPROJ_RST_BITS) as u32) as i32;
                }
            }
        }
        let (w0, w1) = (xqd[0] as i32, xqd[1] as i32);
        let w2 = (1 << SGRPROJ_PRJ_BITS) - w0 - w1;
        let sh = (SGRPROJ_RST_BITS + SGRPROJ_PRJ_BITS) as u32;
        for i in 0..h as usize {
            for j in 0..w as usize {
                let u = (ctx.cdef.row(y as usize + i)[x as usize + j] as i32) << SGRPROJ_RST_BITS;
                let mut v = w1 * u;
                v += if SGR_PARAMS[set][0] != 0 { w0 * flt[0][i][j] } else { w0 * u };
                v += if SGR_PARAMS[set][2] != 0 { w2 * flt[1][i][j] } else { w2 * u };
                out.set(x as usize + j, y as usize + i, ((v + (1 << (sh - 1))) >> sh).clamp(0, (1 << bd) - 1) as u16);
            }
        }
    }

    /// Rectangles of 4x4 (luma) or 2x2 (4:2:0 chroma) blocks in random pictures, near the
    /// picture's edges and next to a stripe's edges, filtered whole and block by block.
    #[test]
    fn rectangles_match_the_spec_blocks() {
        let mut seed = 0x1234_5678u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let (pw, ph) = (72usize, 70usize);
        for case in 0..1500 {
            let bd = [8, 10, 12][case % 3];
            let max = (1u32 << bd) - 1;
            // Smooth pictures with noise, or pure noise.
            let smooth = next() % 2 == 0;
            let mut pic = |_: ()| {
                let mut p = Plane::new(pw, ph);
                let base = next() % (max + 1);
                for (i, v) in p.data.iter_mut().enumerate() {
                    let n = next() % (max + 1);
                    *v = if smooth { ((base as usize + (i % pw) * 3 + (i / pw) * 2 + (n as usize & 15)) % (max as usize + 1)) as u16 } else { n as u16 };
                }
                p
            };
            let (cur, cdef) = (pic(()), pic(()));
            let blk = if next() % 3 == 0 { 2 } else { 4 };
            let (plane_end_x, plane_end_y) = (pw as i32 - 1 - (next() % 5) as i32, ph as i32 - 1 - (next() % 5) as i32);
            let x = blk * (next() as usize % (60 / blk));
            let y = blk * (next() as usize % (60 / blk));
            let w = (blk * (1 + next() as usize % 6)).min(plane_end_x as usize + 1 - x);
            let h = (blk * (1 + next() as usize % 6)).min(plane_end_y as usize + 1 - y);
            // A stripe holding the rectangle (as the decoder's rectangles are), starting up to 7
            // rows above it: the rows read above and below it often lie outside the stripe.
            let stripe_start_y = y as i32 - (next() % 8) as i32;
            let edges = StripeEdges::save(&cur, stripe_start_y, 32, ph as i32);
            let ctx = LrCtx {
                edges: &edges,
                cdef: Rows { data: &cdef.data, stride: pw, y0: 0 },
                plane_end_x,
                plane_end_y,
                stripe_start_y,
                stripe_end_y: stripe_start_y + 31,
            };
            let sgr = next() % 2 == 0;
            let set = next() as usize % 16;
            let xqd = [(next() % 128) as i8 - 96, (next() % 128) as i8 - 32];
            let c = |next: &mut dyn FnMut() -> u32, lo: i32, hi: i32| (lo + (next() % (hi - lo + 1) as u32) as i32) as i8;
            let coef =
                [[c(&mut next, -5, 10), c(&mut next, -23, 8), c(&mut next, -17, 46)], [c(&mut next, -5, 10), c(&mut next, -23, 8), c(&mut next, -17, 46)]];
            let mut want = cdef.clone();
            for by in (y..y + h).step_by(blk) {
                for bx in (x..x + w).step_by(blk) {
                    let (bw, bh) = (blk.min(x + w - bx) as i32, blk.min(y + h - by) as i32);
                    if sgr {
                        spec_self_guided(set, xqd, &ctx, &mut want, (bx as i32, by as i32, bw, bh), bd);
                    } else {
                        spec_wiener(&coef, &ctx, &mut want, (bx as i32, by as i32, bw, bh), bd);
                    }
                }
            }
            let mut got = cdef.clone();
            let mut band = RowBand { data: &mut got.data, stride: pw, y0: 0 };
            let mut s = LrScratch::default();
            ctx.fetch(x as i32, y as i32, w, h, &mut s.src);
            let rect = Rect { x, y, w, h, unit: 0, sgr };
            if sgr {
                self_guided(set, xqd, &mut s, &mut band, &rect, bd);
            } else {
                wiener(&coef, &mut s, &mut band, &rect, bd);
            }
            assert!(got.data == want.data, "case {case}: sgr {sgr} set {set} bd {bd} rect ({x}, {y}, {w}, {h}) blocks {blk}");
        }
    }
}
