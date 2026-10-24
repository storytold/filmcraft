//! CDEF (7.15).

use crate::frame::{FrameBuf, Plane, RowBand};
use crate::par::Pool;
use crate::postfilter::bands;
use crate::spec_tables::*;
use crate::state::FrameState;

/// Run CDEF on `fs.cur`; returns the CdefFrame, or None when no block is filtered (CdefFrame
/// equals CurrFrame). Bands of 64 luma rows (whole 64x64 filter blocks) are filtered in
/// parallel: every block reads only CurrFrame and writes only its own samples.
pub(crate) fn apply(fs: &FrameState, pool: &Pool) -> Option<FrameBuf> {
    let fh = &fs.fh;
    if fh.coded_lossless || fh.allow_intrabc || !fs.seq.enable_cdef {
        return None;
    }
    let (mi_rows, mi_cols) = (fh.mi_rows as usize, fh.mi_cols as usize);
    let fb_rows = mi_rows.div_ceil(16);
    let fb_cols = mi_cols.div_ceil(16);
    if !(0..fb_rows).any(|fr| (0..fb_cols).any(|fc| fb_skip_mask(fs, fr * 16, fc * 16).is_some())) {
        return None;
    }
    let mut out = fs.cur.clone();
    let ranges = bands(pool, mi_rows * 4);
    let mut planes = out.row_bands(&ranges);
    pool.map_mut(&mut planes, |b, band| {
        let (start, end) = ranges[b];
        let mut buf = [0i16; BUF_LEN];
        for fr in start / 64..fb_rows.min(end.div_ceil(64)) {
            for fc in 0..fb_cols {
                if let Some(mask) = fb_skip_mask(fs, fr * 16, fc * 16) {
                    cdef_fb(fs, band, fr * 16, fc * 16, mask, &mut buf);
                }
            }
        }
    });
    drop(planes);
    Some(out)
}

/// The filtered 8x8 blocks of the 64x64 filter block at (r, c): bit `(i << 3) | j` is set for
/// block row `i`, column `j` when it is not skipped. None when the filter block is off
/// (cdef_idx -1) or every block in it is skipped.
fn fb_skip_mask(fs: &FrameState, r: usize, c: usize) -> Option<u64> {
    if fs.cdef_idx(r, c) < 0 {
        return None;
    }
    let mi = &fs.mi;
    let (mi_rows, mi_cols) = (fs.fh.mi_rows as usize, fs.fh.mi_cols as usize);
    let sk = |rr: usize, cc: usize| -> bool { if rr < mi.rows && cc < mi.cols { mi.skip[mi.idx(rr, cc)] } else { true } };
    let mut mask = 0u64;
    for i in 0..(mi_rows - r).min(16).div_ceil(2) {
        let rr = r + 2 * i;
        for j in 0..(mi_cols - c).min(16).div_ceil(2) {
            let cc = c + 2 * j;
            if !(sk(rr, cc) && sk(rr + 1, cc) && sk(rr, cc + 1) && sk(rr + 1, cc + 1)) {
                mask |= 1 << (i * 8 + j);
            }
        }
    }
    (mask != 0).then_some(mask)
}

fn cdef_fb(fs: &FrameState, out: &mut [RowBand; 3], r: usize, c: usize, mask: u64, buf: &mut [i16; BUF_LEN]) {
    let idx = fs.cdef_idx(r, c) as usize;
    let coeff_shift = fs.bit_depth - 8;
    let cp = &fs.fh.cdef;
    let (y_pri, y_sec) = ((cp.y_pri[idx] << coeff_shift) as i32, (cp.y_sec[idx] << coeff_shift) as i32);
    let (uv_pri, uv_sec) = ((cp.uv_pri[idx] << coeff_shift) as i32, (cp.uv_sec[idx] << coeff_shift) as i32);
    let chroma = fs.num_planes > 1 && (uv_pri != 0 || uv_sec != 0);
    if y_pri == 0 && y_sec == 0 && !chroma {
        // Every tap contributes 0 and the clamp to [min, max] keeps x: CdefFrame = CurrFrame.
        return;
    }
    // Luma direction and variance of every filtered 8x8 block (only used with a primary
    // strength).
    let mut dirs = [(0usize, 0i32); 64];
    if y_pri != 0 || (chroma && uv_pri != 0) {
        let mut m = mask;
        while m != 0 {
            let k = m.trailing_zeros() as usize;
            m &= m - 1;
            dirs[k] = direction(fs, r + 2 * (k >> 3), c + 2 * (k & 7));
        }
    }
    if y_pri != 0 || y_sec != 0 {
        let damping = cp.damping as i32 + coeff_shift as i32;
        fill(fs, 0, r, c, buf);
        let mut m = mask;
        while m != 0 {
            let k = m.trailing_zeros() as usize;
            m &= m - 1;
            let (y_dir, var) = dirs[k];
            let dir = if y_pri == 0 { 0 } else { y_dir };
            let var_str = if (var >> 6) != 0 { (31 - ((var >> 6) as u32).leading_zeros()).min(12) as i32 } else { 0 };
            let pri = if var != 0 { (y_pri * (4 + var_str) + 8) >> 4 } else { 0 };
            filter(fs, buf, out, 0, (r, c), k, pri, y_sec, damping, dir);
        }
    }
    if !chroma {
        return;
    }
    let damping = cp.damping as i32 + coeff_shift as i32 - 1;
    for plane in 1..3 {
        fill(fs, plane, r, c, buf);
        let mut m = mask;
        while m != 0 {
            let k = m.trailing_zeros() as usize;
            m &= m - 1;
            let dir = if uv_pri == 0 { 0 } else { CDEF_UV_DIR[fs.ssx][fs.ssy][dirs[k].0] as usize };
            filter(fs, buf, out, plane, (r, c), k, uv_pri, uv_sec, damping, dir);
        }
    }
}

fn direction(fs: &FrameState, r: usize, c: usize) -> (usize, i32) {
    let mut cost = [0i32; 8];
    let mut partial = [[0i32; 15]; 8];
    let x0 = c << 2;
    let y0 = r << 2;
    let pl = &fs.cur.planes[0];
    let sh = fs.bit_depth - 8;
    // Each line sum gathers whole rows: partial[d][o + j] += row[j] at a row-dependent offset
    // o (reversed or pairwise summed rows for directions 1, 3 and 4).
    let add = |p: &mut [i32; 15], o: usize, v: &[i32]| {
        for (d, &s) in p[o..o + v.len()].iter_mut().zip(v) {
            *d += s;
        }
    };
    for i in 0..8 {
        let Some(row) = pl.row(y0 + i).get(x0..).and_then(|r| r.first_chunk::<8>()) else {
            continue;
        };
        let x: [i32; 8] = std::array::from_fn(|j| (row[j] as i32 >> sh) - 128);
        let rev: [i32; 8] = std::array::from_fn(|j| x[7 - j]);
        let pair: [i32; 4] = std::array::from_fn(|k| x[2 * k] + x[2 * k + 1]);
        let pair_rev: [i32; 4] = std::array::from_fn(|k| pair[3 - k]);
        add(&mut partial[0], i, &x);
        add(&mut partial[1], i, &pair);
        partial[2][i] = x.iter().sum();
        add(&mut partial[3], i, &pair_rev);
        add(&mut partial[4], i, &rev);
        add(&mut partial[5], 3 - i / 2, &x);
        add(&mut partial[6], 0, &x);
        add(&mut partial[7], i / 2, &x);
    }
    for i in 0..8 {
        cost[2] += partial[2][i] * partial[2][i];
        cost[6] += partial[6][i] * partial[6][i];
    }
    cost[2] *= DIV_TABLE[8] as i32;
    cost[6] *= DIV_TABLE[8] as i32;
    for i in 0..7 {
        cost[0] += (partial[0][i] * partial[0][i] + partial[0][14 - i] * partial[0][14 - i]) * DIV_TABLE[i + 1] as i32;
        cost[4] += (partial[4][i] * partial[4][i] + partial[4][14 - i] * partial[4][14 - i]) * DIV_TABLE[i + 1] as i32;
    }
    cost[0] += partial[0][7] * partial[0][7] * DIV_TABLE[8] as i32;
    cost[4] += partial[4][7] * partial[4][7] * DIV_TABLE[8] as i32;
    let mut i = 1;
    while i < 8 {
        for j in 0..5 {
            cost[i] += partial[i][3 + j] * partial[i][3 + j];
        }
        cost[i] *= DIV_TABLE[8] as i32;
        for j in 0..3 {
            cost[i] += (partial[i][j] * partial[i][j] + partial[i][10 - j] * partial[i][10 - j]) * DIV_TABLE[2 * j + 2] as i32;
        }
        i += 2;
    }
    let mut best = 0;
    let mut y_dir = 0;
    for (i, &c) in cost.iter().enumerate() {
        if c > best {
            best = c;
            y_dir = i;
        }
    }
    (y_dir, (best - cost[(y_dir + 4) & 7]) >> 10)
}

/// Marks a sample outside the frame (CdefAvailable = 0) in the padded block.
const CDEF_NA: i16 = i16::MIN;
/// Padded filter block: up to 64x64 samples plus 2 on every side.
const PAD_STRIDE: usize = 64 + 4;
const BUF_LEN: usize = PAD_STRIDE * PAD_STRIDE;

/// Samples in plane coordinates for which CdefAvailable is 1 (their 4x4 block is inside the
/// frame): columns and rows below these limits.
fn avail_limits(fs: &FrameState, plane: usize) -> (usize, usize) {
    let (sub_x, sub_y) = if plane > 0 { (fs.ssx, fs.ssy) } else { (0, 0) };
    ((fs.fh.mi_cols as usize * 4) >> sub_x, (fs.fh.mi_rows as usize * 4) >> sub_y)
}

/// Copy CurrFrame around the filter block at (r, c) of `plane` into `buf` as i16, with
/// [`CDEF_NA`] for unavailable samples. `buf[(i + 2) * PAD_STRIDE + j + 2]` is the sample at
/// row `i`, column `j` of the filter block.
fn fill(fs: &FrameState, plane: usize, r: usize, c: usize, buf: &mut [i16; BUF_LEN]) {
    let (sub_x, sub_y) = if plane > 0 { (fs.ssx, fs.ssy) } else { (0, 0) };
    let (x0, y0) = ((c * 4) >> sub_x, (r * 4) >> sub_y);
    let (w, h) = ((64 >> sub_x) + 4, (64 >> sub_y) + 4);
    let (avail_w, avail_h) = avail_limits(fs, plane);
    let src: &Plane = &fs.cur.planes[plane];
    // Columns [lo, hi) of the padded rows are available.
    let lo = 2usize.saturating_sub(x0);
    let hi = (avail_w + 2).saturating_sub(x0).min(w).max(lo);
    for (i, row) in buf.as_chunks_mut::<PAD_STRIDE>().0.iter_mut().take(h).enumerate() {
        let y = (y0 + i).wrapping_sub(2);
        if y >= avail_h {
            row[..w].fill(CDEF_NA);
            continue;
        }
        let s = &src.row(y)[x0 + lo - 2..x0 + hi - 2];
        row[..lo].fill(CDEF_NA);
        for (d, &v) in row[lo..hi].iter_mut().zip(s) {
            *d = v as i16;
        }
        row[hi..w].fill(CDEF_NA);
    }
}

#[inline(always)]
fn constrain(diff: i32, threshold: i32, adj: u32) -> i32 {
    let mag = (threshold - (diff.abs() >> adj)).clamp(0, diff.abs());
    if diff < 0 { -mag } else { mag }
}

/// Filter the 8x8 block `k` (row `k >> 3`, column `k & 7`) of the filter block at `fb`, with
/// `buf` filled for `plane` by [`fill`].
#[allow(clippy::too_many_arguments)]
fn filter(
    fs: &FrameState,
    buf: &[i16; BUF_LEN],
    out: &mut [RowBand; 3],
    plane: usize,
    fb: (usize, usize),
    k: usize,
    pri: i32,
    sec: i32,
    damping: i32,
    dir: usize,
) {
    if pri == 0 && sec == 0 {
        // Every tap contributes 0 and the clamp to [min, max] keeps x: CdefFrame = CurrFrame.
        return;
    }
    let coeff_shift = fs.bit_depth - 8;
    let (sub_x, sub_y) = if plane > 0 { (fs.ssx, fs.ssy) } else { (0, 0) };
    let w = 8 >> sub_x;
    let h = 8 >> sub_y;
    // Position in the filter block and in the plane.
    let (bx, by) = ((k & 7) * w, (k >> 3) * h);
    let x0 = ((fb.1 * 4) >> sub_x) + bx;
    let y0 = ((fb.0 * 4) >> sub_y) + by;
    let (avail_w, avail_h) = avail_limits(fs, plane);
    let all_avail = x0 >= 2 && y0 >= 2 && x0 + w + 2 <= avail_w && y0 + h + 2 <= avail_h;
    let pri_tap = ((pri >> coeff_shift) & 1) as usize;
    let off = |d: usize, k: usize| CDEF_DIRECTIONS[d][k][0] as isize * PAD_STRIDE as isize + CDEF_DIRECTIONS[d][k][1] as isize;
    let pri_off = [off(dir, 0), off(dir, 1)];
    let d_lo = (dir + 6) & 7;
    let d_hi = (dir + 2) & 7;
    let sec_off = [[off(d_lo, 0), off(d_hi, 0)], [off(d_lo, 1), off(d_hi, 1)]];
    let pri_taps = [CDEF_PRI_TAPS[pri_tap][0] as i32, CDEF_PRI_TAPS[pri_tap][1] as i32];
    let sec_taps = [CDEF_SEC_TAPS[pri_tap][0] as i32, CDEF_SEC_TAPS[pri_tap][1] as i32];
    let adj = |t: i32| if t == 0 { 0 } else { (damping - (31 - (t as u32).leading_zeros()) as i32).max(0) as u32 };
    let (pri_adj, sec_adj) = (adj(pri), adj(sec));
    let dst = &mut out[plane];
    let base = (by + 2) * PAD_STRIDE + bx + 2;
    if all_avail {
        // Every tap is available: whole rows at a time (constrain() with threshold 0 is 0).
        let mut taps = [(0isize, 0i16, 0i16, 0u32); 12];
        let mut n = 0;
        for k in 0..2 {
            for sign in [-1isize, 1] {
                taps[n] = (sign * pri_off[k], pri_taps[k] as i16, pri as i16, pri_adj);
                taps[n + 1] = (sign * sec_off[k][0], sec_taps[k] as i16, sec as i16, sec_adj);
                taps[n + 2] = (sign * sec_off[k][1], sec_taps[k] as i16, sec as i16, sec_adj);
                n += 3;
            }
        }
        let rows = (base, y0, x0, h);
        match w {
            8 => filter_rows::<8>(buf, &taps, dst, rows),
            _ => filter_rows::<4>(buf, &taps, dst, rows),
        }
        return;
    }
    let src = &fs.cur.planes[plane];
    for i in 0..h {
        // The centre sample is read even where CdefAvailable is 0 (8x8 blocks straddling an odd
        // MiRows / MiCols edge).
        let srow = &src.row(y0 + i)[x0..x0 + w];
        let drow = &mut dst.row_mut(y0 + i)[x0..x0 + w];
        for j in 0..w {
            let ci = (base + i * PAD_STRIDE + j) as isize;
            let x = srow[j] as i32;
            let mut sum = 0i32;
            let mut max = x;
            let mut min = x;
            for k in 0..2 {
                for sign in [-1isize, 1] {
                    let p = buf[(ci + sign * pri_off[k]) as usize];
                    if p != CDEF_NA {
                        let p = p as i32;
                        if pri != 0 {
                            sum += pri_taps[k] * constrain(p - x, pri, pri_adj);
                        }
                        max = max.max(p);
                        min = min.min(p);
                    }
                    for s in 0..2 {
                        let q = buf[(ci + sign * sec_off[k][s]) as usize];
                        if q != CDEF_NA {
                            let q = q as i32;
                            if sec != 0 {
                                sum += sec_taps[k] * constrain(q - x, sec, sec_adj);
                            }
                            max = max.max(q);
                            min = min.min(q);
                        }
                    }
                }
            }
            drow[j] = (x + ((8 + sum - (sum < 0) as i32) >> 4)).clamp(min, max) as u16;
        }
    }
}

/// CDEF of a block whose taps are all available, `W` samples per row, its top-left sample at
/// `buf[base]`. Samples (at most 12 bits), differences and the weighted tap sum all fit in
/// i16, which doubles the SIMD width.
#[inline(always)]
fn filter_rows<const W: usize>(buf: &[i16; BUF_LEN], taps: &[(isize, i16, i16, u32); 12], dst: &mut RowBand, (base, y0, x0, h): (usize, usize, usize, usize)) {
    for i in 0..h {
        let ci = (base + i * PAD_STRIDE) as isize;
        let Some(&x) = buf.get(ci as usize..).and_then(|r| r.first_chunk::<W>()) else {
            return;
        };
        let (mut sum, mut max, mut min) = ([0i16; W], x, x);
        for &(off, weight, thr, adj) in taps {
            let start = (ci + off) as usize;
            let Some(p) = buf.get(start..).and_then(|r| r.first_chunk::<W>()) else {
                return;
            };
            for j in 0..W {
                // constrain(): .max / .min instead of clamp (whose bound check panics), and
                // the sign applied branch-free, so the loop vectorises.
                let d = p[j] - x[j];
                let ad = d.abs();
                let mag = (thr - (ad >> adj)).max(0).min(ad);
                let sign = d >> 15;
                sum[j] += weight * ((mag ^ sign) - sign);
                max[j] = max[j].max(p[j]);
                min[j] = min[j].min(p[j]);
            }
        }
        let drow = &mut dst.row_mut(y0 + i)[x0..x0 + W];
        for j in 0..W {
            let s = sum[j];
            drow[j] = (x[j] + ((8 + s - (s < 0) as i16) >> 4)).max(min[j]).min(max[j]) as u16;
        }
    }
}
