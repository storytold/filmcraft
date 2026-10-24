//! Super-resolution upscaling (7.16) and loop restoration (7.17).

use crate::frame::{FrameBuf, Plane, RowBand};
use crate::par::Pool;
use crate::postfilter::bands;
use crate::spec_tables::*;
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

/// One plane's loop restoration inputs: UpscaledCurrFrame and UpscaledCdefFrame, the plane
/// size and the restoration units.
struct LrPlane<'a> {
    cur: &'a Plane,
    cdef: &'a Plane,
    width: usize,
    height: usize,
    sub_y: usize,
    bit_depth: u32,
    unit_size: usize,
    unit_rows: usize,
    unit_cols: usize,
    lr_type: &'a [u8],
    wiener: &'a [[[i8; 3]; 2]],
    sgr_set: &'a [u8],
    sgr_xqd: &'a [[i8; 2]],
}

/// Loop restoration (7.17): returns LrFrame given UpscaledCurrFrame and UpscaledCdefFrame.
/// Bands of 64 luma rows are restored in parallel: every unit reads only the two input frames
/// and writes only its own samples.
pub(crate) fn loop_restoration(fs: &FrameState, cur: &FrameBuf, cdef: &FrameBuf, pool: &Pool) -> FrameBuf {
    let fh = &fs.fh;
    let mut lr = cdef.clone();
    let ranges = bands(pool, fh.frame_height as usize);
    let mut planes = lr.row_bands(&ranges);
    pool.map_mut(&mut planes, |b, band| {
        let (start, end) = ranges[b];
        let mut scratch = Scratch::default();
        for (plane, out) in band.iter_mut().enumerate().take(fs.num_planes) {
            if fh.lr.frame_restoration_type[plane] == RESTORE_NONE as u8 {
                continue;
            }
            let (sub_x, sub_y) = if plane > 0 { (fs.ssx, fs.ssy) } else { (0, 0) };
            let p = LrPlane {
                cur: &cur.planes[plane],
                cdef: &cdef.planes[plane],
                width: (fh.upscaled_width as usize + sub_x) >> sub_x,
                height: (fh.frame_height as usize + sub_y) >> sub_y,
                sub_y,
                bit_depth: fs.bit_depth,
                unit_size: fh.lr.loop_restoration_size[plane] as usize,
                unit_rows: fs.lr_unit_rows[plane],
                unit_cols: fs.lr_unit_cols[plane],
                lr_type: &fs.lr.lr_type[plane],
                wiener: &fs.lr.lr_wiener[plane],
                sgr_set: &fs.lr.lr_sgr_set[plane],
                sgr_xqd: &fs.lr.lr_sgr_xqd[plane],
            };
            p.restore(out, start >> sub_y, end >> sub_y, &mut scratch);
        }
    });
    drop(planes);
    lr
}

/// Row buffers reused across stripes.
#[derive(Default)]
struct Scratch {
    /// Source rows of a stripe segment, 3 rows above and below and 3 columns left and right.
    src: Vec<i32>,
    /// Wiener horizontal pass output, or the self-guided A array.
    inter: Vec<i32>,
    /// Self-guided B array.
    b: Vec<i32>,
    /// Self-guided filter outputs of both passes.
    flt: [Vec<i32>; 2],
    /// Column sums of samples and squared samples over a box's rows.
    col: Vec<i32>,
    col_sq: Vec<i32>,
}

/// Padding of the source rows on every side.
const PAD: usize = 3;

impl LrPlane<'_> {
    /// The row of the get source sample process (7.17.6) for row `y` of the stripe
    /// [`stripe_start`, `stripe_end`]: pre-CDEF rows (at most 2 beyond the stripe) outside it.
    fn source_row(&self, y: isize, stripe_start: isize, stripe_end: isize) -> &[u16] {
        let y = y.min(self.height as isize - 1).max(0);
        if y < stripe_start {
            self.cur.row((stripe_start - 2).max(y) as usize)
        } else if y > stripe_end {
            self.cur.row((stripe_end + 2).min(y) as usize)
        } else {
            self.cdef.row(y as usize)
        }
    }

    /// Restore rows [y0, y1) of the plane into `out`, in segments of rows that share a stripe
    /// and a unit row.
    fn restore(&self, out: &mut RowBand, y0: usize, y1: usize, s: &mut Scratch) {
        let y1 = y1.min(self.height);
        let off = 8 >> self.sub_y;
        let stripe_h = 64 >> self.sub_y;
        let mut y = y0;
        while y < y1 {
            // Stripes start 8 luma rows above multiples of 64 luma rows.
            let stripe_num = (y + off) / stripe_h;
            let next_stripe = (stripe_num + 1) * stripe_h - off;
            let stripe = ((stripe_num * stripe_h) as isize - off as isize, next_stripe as isize - 1);
            let unit_row = ((y + off) / self.unit_size).min(self.unit_rows - 1);
            let mut end = y1.min(next_stripe);
            if unit_row + 1 < self.unit_rows {
                end = end.min((unit_row + 1) * self.unit_size - off);
            }
            self.segment(out, (y, end), stripe, unit_row, s);
            y = end;
        }
    }

    fn segment(&self, out: &mut RowBand, (ya, yb): (usize, usize), stripe: (isize, isize), unit_row: usize, s: &mut Scratch) {
        let w = self.width;
        let stride = w + 2 * PAD;
        let rows = yb - ya + 2 * PAD;
        s.src.clear();
        s.src.resize(rows * stride, 0);
        for (k, dst) in s.src.chunks_exact_mut(stride).enumerate() {
            let src = &self.source_row(ya as isize - PAD as isize + k as isize, stripe.0, stripe.1)[..w];
            dst[..PAD].fill(src[0] as i32);
            for (d, &v) in dst[PAD..PAD + w].iter_mut().zip(src) {
                *d = v as i32;
            }
            dst[PAD + w..].fill(src[w - 1] as i32);
        }
        for uc in 0..self.unit_cols {
            let xa = uc * self.unit_size;
            let xb = if uc + 1 == self.unit_cols { w } else { ((uc + 1) * self.unit_size).min(w) };
            if xa >= xb {
                continue;
            }
            let ui = unit_row * self.unit_cols + uc;
            match self.lr_type[ui] {
                t if t == RESTORE_WIENER as u8 => self.wiener(out, (ya, yb), (xa, xb), ui, s),
                t if t == RESTORE_SGRPROJ as u8 => self.self_guided(out, (ya, yb), (xa, xb), ui, s),
                _ => {}
            }
        }
    }

    fn wiener(&self, out: &mut RowBand, (ya, yb): (usize, usize), (xa, xb): (usize, usize), ui: usize, s: &mut Scratch) {
        let bd = self.bit_depth;
        let round0: u32 = if bd == 12 { 5 } else { 3 };
        let round1: u32 = if bd == 12 { 9 } else { 11 };
        let coef = &self.wiener[ui];
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
        let stride = self.width + 2 * PAD;
        let (w, h) = (xb - xa, yb - ya);
        s.inter.clear();
        s.inter.resize((h + 6) * w, 0);
        for (src, inter) in s.src.chunks_exact(stride).zip(s.inter.chunks_exact_mut(w)) {
            // Output column x reads source columns x - 3 ..= x + 3, padded index x ..= x + 6.
            let src = &src[xa..xa + w + 6];
            for (c, v) in inter.iter_mut().enumerate() {
                let t = &src[c..c + 7];
                let mut sum = 0;
                for k in 0..7 {
                    sum += hfilter[k] * t[k];
                }
                *v = ((sum + (1 << (round0 - 1))) >> round0).clamp(-offset, limit - offset);
            }
        }
        let max = (1i32 << bd) - 1;
        for r in 0..h {
            let drow = &mut out.row_mut(ya + r)[xa..xb];
            let col = &s.inter[r * w..(r + 7) * w];
            for (c, d) in drow.iter_mut().enumerate() {
                let mut sum = 0;
                for k in 0..7 {
                    sum += vfilter[k] * col[k * w + c];
                }
                *d = ((sum + (1 << (round1 - 1))) >> round1).clamp(0, max) as u16;
            }
        }
    }

    fn self_guided(&self, out: &mut RowBand, (ya, yb): (usize, usize), (xa, xb): (usize, usize), ui: usize, s: &mut Scratch) {
        let set = self.sgr_set[ui] as usize;
        let (w, h) = (xb - xa, yb - ya);
        for pass in 0..2 {
            if SGR_PARAMS[set][pass * 2] != 0 {
                self.box_filter(ya, yb, xa, xb, set, pass, s);
            }
        }
        let xqd = self.sgr_xqd[ui];
        let w0 = xqd[0] as i32;
        let w1 = xqd[1] as i32;
        let w2 = (1 << SGRPROJ_PRJ_BITS) - w0 - w1;
        let r0 = SGR_PARAMS[set][0];
        let r1 = SGR_PARAMS[set][2];
        let max = (1i32 << self.bit_depth) - 1;
        let sh = (SGRPROJ_RST_BITS + SGRPROJ_PRJ_BITS) as u32;
        for i in 0..h {
            let crow = &self.cdef.row(ya + i)[xa..xb];
            let drow = &mut out.row_mut(ya + i)[xa..xb];
            for j in 0..w {
                let u = (crow[j] as i32) << SGRPROJ_RST_BITS;
                let mut v = w1 * u;
                v += if r0 != 0 { w0 * s.flt[0][i * w + j] } else { w0 * u };
                v += if r1 != 0 { w2 * s.flt[1][i * w + j] } else { w2 * u };
                let s = (v + (1 << (sh - 1))) >> sh;
                drow[j] = s.clamp(0, max) as u16;
            }
        }
    }

    /// Box filter process (7.17.3) for one pass over rows [ya, yb) and columns [xa, xb), into
    /// `s.flt[pass]`. A and B are computed for the rows and columns around the block once,
    /// from running box sums over the source rows.
    #[allow(clippy::too_many_arguments)]
    fn box_filter(&self, ya: usize, yb: usize, xa: usize, xb: usize, set: usize, pass: usize, s: &mut Scratch) {
        let bd = self.bit_depth;
        let r = SGR_PARAMS[set][pass * 2] as usize;
        let eps = SGR_PARAMS[set][pass * 2 + 1] as i64;
        let n = ((2 * r + 1) * (2 * r + 1)) as i64;
        let n2e = n * n * eps;
        let sc = ((1i64 << SGRPROJ_MTABLE_BITS) + n2e / 2) / n2e;
        let one_over_n = ((1i64 << SGRPROJ_RECIP_BITS) + n / 2) / n;
        let stride = self.width + 2 * PAD;
        let (w, h) = (xb - xa, yb - ya);
        // A and B at rows ya - 1 ..= yb and columns xa - 1 ..= xb.
        let (aw, ah) = (w + 2, h + 2);
        s.inter.clear();
        s.inter.resize(aw * ah, 0);
        s.b.clear();
        s.b.resize(aw * ah, 0);
        let taps = 2 * r + 1;
        s.col.clear();
        s.col.resize(aw + 2 * r, 0);
        s.col_sq.clear();
        s.col_sq.resize(aw + 2 * r, 0);
        // Padded column of the first source sample of A column 0 (frame column xa - 1 - r).
        let p0 = xa + PAD - 1 - r;
        for i in 0..ah {
            // Pass 0 only reads A and B on odd rows.
            if pass == 0 && ((ya + i + 1) & 1) == 0 {
                continue;
            }
            // Source row of frame row ya - 1 + i - r.
            let k0 = i + PAD - 1 - r;
            s.col.fill(0);
            s.col_sq.fill(0);
            for k in k0..k0 + taps {
                let row = &s.src[k * stride + p0..k * stride + p0 + aw + 2 * r];
                for ((c, q), &v) in s.col.iter_mut().zip(s.col_sq.iter_mut()).zip(row) {
                    *c += v;
                    *q += v * v;
                }
            }
            let (mut b, mut a) = (0i32, 0i32);
            for t in 0..taps - 1 {
                b += s.col[t];
                a += s.col_sq[t];
            }
            for j in 0..aw {
                b += s.col[j + taps - 1];
                a += s.col_sq[j + taps - 1];
                let (a64, b64) = (a as i64, b as i64);
                let aa = round2_64(a64, 2 * (bd - 8));
                let d = round2_64(b64, bd - 8);
                let p = (aa * n - d * d).max(0);
                let z = round2_64(p * sc, SGRPROJ_MTABLE_BITS as u32);
                let a2 = if z >= 255 {
                    256
                } else if z == 0 {
                    1
                } else {
                    ((z << SGRPROJ_SGR_BITS) + z / 2) / (z + 1)
                };
                let b2 = ((1i64 << SGRPROJ_SGR_BITS) - a2) * b64 * one_over_n;
                s.inter[i * aw + j] = a2 as i32;
                s.b[i * aw + j] = round2_64(b2, SGRPROJ_RECIP_BITS as u32) as i32;
                b -= s.col[j];
                a -= s.col_sq[j];
            }
        }
        let flt = &mut s.flt[pass];
        flt.clear();
        flt.resize(w * h, 0);
        let (aa, bb) = (&s.inter, &s.b);
        for i in 0..h {
            let crow = &self.cdef.row(ya + i)[xa..xb];
            let (up, mid, down) = (i * aw, (i + 1) * aw, (i + 2) * aw);
            let odd = ((ya + i) & 1) == 1;
            let shift = if pass == 0 && odd { 4 } else { 5 };
            for j in 0..w {
                let (a, b) = if pass == 0 {
                    if odd {
                        (6 * aa[mid + j + 1] + 5 * (aa[mid + j] + aa[mid + j + 2]), 6 * bb[mid + j + 1] + 5 * (bb[mid + j] + bb[mid + j + 2]))
                    } else {
                        (
                            6 * (aa[up + j + 1] + aa[down + j + 1]) + 5 * (aa[up + j] + aa[up + j + 2] + aa[down + j] + aa[down + j + 2]),
                            6 * (bb[up + j + 1] + bb[down + j + 1]) + 5 * (bb[up + j] + bb[up + j + 2] + bb[down + j] + bb[down + j + 2]),
                        )
                    }
                } else {
                    (
                        4 * (aa[up + j + 1] + aa[mid + j] + aa[mid + j + 1] + aa[mid + j + 2] + aa[down + j + 1])
                            + 3 * (aa[up + j] + aa[up + j + 2] + aa[down + j] + aa[down + j + 2]),
                        4 * (bb[up + j + 1] + bb[mid + j] + bb[mid + j + 1] + bb[mid + j + 2] + bb[down + j + 1])
                            + 3 * (bb[up + j] + bb[up + j + 2] + bb[down + j] + bb[down + j + 2]),
                    )
                };
                let v = a as i64 * crow[j] as i64 + b as i64;
                flt[i * w + j] = round2_64(v, (SGRPROJ_SGR_BITS + shift - SGRPROJ_RST_BITS) as u32) as i32;
            }
        }
    }
}

#[inline(always)]
fn round2_64(x: i64, n: u32) -> i64 {
    if n == 0 { x } else { (x + (1i64 << (n - 1))) >> n }
}

#[cfg(test)]
mod tests {
    //! The stripe implementation against the per-4x4 block one it replaced.
    use super::*;

    struct LrCtx<'a> {
        cur: &'a Plane,
        cdef: &'a Plane,
        plane_end_x: i32,
        plane_end_y: i32,
        stripe_start_y: i32,
        stripe_end_y: i32,
    }

    impl LrCtx<'_> {
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

    /// The former loop over 4x4 luma blocks for a frame of `up_w` x `fr_h` luma samples.
    fn reference(p: &LrPlane, sub_x: i32, up_w: i32, fr_h: i32, out: &mut Plane) {
        let sub_y = p.sub_y as i32;
        let bd = p.bit_depth;
        let unit_size = p.unit_size as i32;
        let (unit_rows, unit_cols) = (p.unit_rows as i32, p.unit_cols as i32);
        let plane_end_x = ((up_w + sub_x) >> sub_x) - 1;
        let plane_end_y = ((fr_h + sub_y) >> sub_y) - 1;
        let mut row = 0;
        while row * 4 < fr_h {
            let luma_y = row * 4;
            let stripe_num = (luma_y + 8) / 64;
            let stripe_start_y = (-8 + stripe_num * 64) >> sub_y;
            let stripe_end_y = stripe_start_y + (64 >> sub_y) - 1;
            let ctx = LrCtx { cur: p.cur, cdef: p.cdef, plane_end_x, plane_end_y, stripe_start_y, stripe_end_y };
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
                    match p.lr_type[ui] {
                        t if t == RESTORE_WIENER as u8 => wiener(p, &ctx, out, ui, x, y, w, h, bd),
                        t if t == RESTORE_SGRPROJ as u8 => self_guided(p, &ctx, out, ui, x, y, w, h, bd),
                        _ => {}
                    }
                }
                col += 1;
            }
            row += 1;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn wiener(p: &LrPlane, ctx: &LrCtx, out: &mut Plane, ui: usize, x: i32, y: i32, w: i32, h: i32, bd: u32) {
        let round0: u32 = if bd == 12 { 5 } else { 3 };
        let round1: u32 = if bd == 12 { 9 } else { 11 };
        let coef = &p.wiener[ui];
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

    #[allow(clippy::too_many_arguments)]
    fn self_guided(p: &LrPlane, ctx: &LrCtx, out: &mut Plane, ui: usize, x: i32, y: i32, w: i32, h: i32, bd: u32) {
        let set = p.sgr_set[ui] as usize;
        let mut flt0 = [[0i32; 4]; 4];
        let mut flt1 = [[0i32; 4]; 4];
        box_filter(ctx, x, y, w, h, set, 0, bd, &mut flt0);
        box_filter(ctx, x, y, w, h, set, 1, bd, &mut flt1);
        let xqd = p.sgr_xqd[ui];
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

    #[allow(clippy::too_many_arguments)]
    fn box_filter(ctx: &LrCtx, x: i32, y: i32, w: i32, h: i32, set: usize, pass: usize, bd: u32, f: &mut [[i32; 4]; 4]) {
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

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 32) as u32
        }
        fn below(&mut self, n: u32) -> u32 {
            self.next() % n
        }
    }

    /// Random samples: uniform noise, a smooth ramp with noise, or saturated values.
    fn fill(rng: &mut Rng, p: &mut Plane, bd: u32) {
        let max = (1u32 << bd) - 1;
        let kind = rng.below(4);
        let stride = p.stride;
        for (i, v) in p.data.iter_mut().enumerate() {
            let (x, y) = ((i % stride) as u32, (i / stride) as u32);
            *v = match kind {
                0 => rng.below(max + 1),
                1 => ((x * 7 + y * 3) % (max + 1) + rng.below(16)).min(max),
                2 => {
                    if (x + y) % 2 == 0 {
                        0
                    } else {
                        max
                    }
                }
                _ => max - rng.below(4).min(max),
            } as u16;
        }
    }

    fn count_units(unit_size: usize, size: usize) -> usize {
        ((size + (unit_size >> 1)) / unit_size).max(1)
    }

    #[test]
    fn stripes_match_per_block_reference() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let subsampling = [(0, 0), (1, 1), (1, 0)];
        let mut cases = 0;
        for bd in [8u32, 10, 12] {
            for luma_unit in [64usize, 128, 256] {
                for (sub_x, sub_y) in subsampling {
                    for _ in 0..6 {
                        let up_w = 8 + rng.below(300) as i32;
                        let fr_h = 8 + rng.below(300) as i32;
                        // Chroma units of 4:2:0 may be half the luma size (lr_uv_shift).
                        let unit_size = if sub_x == 1 && sub_y == 1 && rng.below(2) == 1 { luma_unit >> 1 } else { luma_unit };
                        let (w, h) = (((up_w + sub_x) >> sub_x) as usize, ((fr_h + sub_y) >> sub_y) as usize);
                        let (unit_rows, unit_cols) = (count_units(unit_size, h), count_units(unit_size, w));
                        let n = unit_rows * unit_cols;
                        let lr_type: Vec<u8> = (0..n).map(|_| rng.below(3) as u8).collect();
                        let wiener: Vec<[[i8; 3]; 2]> = (0..n)
                            .map(|_| {
                                let mut c = || [rng.below(24) as i8 - 5, rng.below(48) as i8 - 23, rng.below(64) as i8 - 17];
                                [c(), c()]
                            })
                            .collect();
                        let sgr_set: Vec<u8> = (0..n).map(|_| rng.below(16) as u8).collect();
                        let sgr_xqd: Vec<[i8; 2]> = (0..n).map(|_| [rng.below(128) as i8 - 96, rng.below(128) as i8 - 32]).collect();
                        let (mut cur, mut cdef) = (Plane::new(w + 16, h + 16), Plane::new(w + 16, h + 16));
                        fill(&mut rng, &mut cur, bd);
                        fill(&mut rng, &mut cdef, bd);
                        let p = LrPlane {
                            cur: &cur,
                            cdef: &cdef,
                            width: w,
                            height: h,
                            sub_y: sub_y as usize,
                            bit_depth: bd,
                            unit_size,
                            unit_rows,
                            unit_cols,
                            lr_type: &lr_type,
                            wiener: &wiener,
                            sgr_set: &sgr_set,
                            sgr_xqd: &sgr_xqd,
                        };
                        let mut want = cdef.clone();
                        reference(&p, sub_x, up_w, fr_h, &mut want);
                        // One band, and bands of 64 luma rows.
                        for band_rows in [usize::MAX, 64] {
                            let ranges: Vec<(usize, usize)> = if band_rows == usize::MAX {
                                vec![(0, usize::MAX)]
                            } else {
                                (0..(fr_h as usize).div_ceil(64)).map(|b| (b * 64, (b + 1) * 64)).collect()
                            };
                            let chroma: Vec<(usize, usize)> = ranges.iter().map(|&(s, e)| (s >> sub_y, e >> sub_y)).collect();
                            let mut got = cdef.clone();
                            let mut scratch = Scratch::default();
                            for (band, &(s, e)) in got.row_bands(&chroma).iter_mut().zip(&chroma) {
                                p.restore(band, s, e, &mut scratch);
                            }
                            for y in 0..h {
                                assert_eq!(
                                    &got.row(y)[..w],
                                    &want.row(y)[..w],
                                    "row {y}: {bd}-bit, unit {unit_size}, subsampling ({sub_x}, {sub_y}), {up_w}x{fr_h}, bands {band_rows}"
                                );
                            }
                        }
                        cases += 1;
                    }
                }
            }
        }
        assert_eq!(cases, 162);
    }
}
