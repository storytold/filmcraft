//! CDEF (7.15).

use crate::frame::{FrameBuf, RowBand};
use crate::par::Pool;
use crate::postfilter::bands;
use crate::spec_tables::*;
use crate::state::FrameState;

/// Run CDEF on `fs.cur`; returns the CdefFrame, or None when no block is filtered (CdefFrame
/// equals CurrFrame). Bands of 64 luma rows (whole 8x8 blocks) are filtered in parallel: every
/// block reads only CurrFrame and writes only its own samples.
pub(crate) fn apply(fs: &FrameState, pool: &Pool) -> Option<FrameBuf> {
    let fh = &fs.fh;
    if fh.coded_lossless || fh.allow_intrabc || !fs.seq.enable_cdef {
        return None;
    }
    let (mi_rows, mi_cols) = (fh.mi_rows as usize, fh.mi_cols as usize);
    let mut out = fs.cur.clone();
    let ranges = bands(pool, mi_rows * 4);
    let mut planes = out.row_bands(&ranges);
    let any = pool.map_mut(&mut planes, |b, band| {
        let (start, end) = ranges[b];
        let mut any = false;
        let mut r = start / 4;
        while r < mi_rows.min(end / 4) {
            let mut c = 0;
            while c < mi_cols {
                let idx = fs.cdef_idx(r & !15, c & !15);
                if idx >= 0 && cdef_block(fs, band, r, c, idx as usize) {
                    any = true;
                }
                c += 2;
            }
            r += 2;
        }
        any
    });
    drop(planes);
    any.contains(&true).then_some(out)
}

fn cdef_block(fs: &FrameState, out: &mut [RowBand; 3], r: usize, c: usize, idx: usize) -> bool {
    let mi = &fs.mi;
    let sk = |rr: usize, cc: usize| -> bool { if rr < mi.rows && cc < mi.cols { mi.skip[mi.idx(rr, cc)] } else { true } };
    let skip = sk(r, c) && sk(r + 1, c) && sk(r, c + 1) && sk(r + 1, c + 1);
    if skip {
        return false;
    }
    let coeff_shift = fs.bit_depth - 8;
    let (y_dir, var) = direction(fs, r, c);
    let cp = &fs.fh.cdef;
    let mut pri = (cp.y_pri[idx] << coeff_shift) as i32;
    let sec = (cp.y_sec[idx] << coeff_shift) as i32;
    let dir = if pri == 0 { 0 } else { y_dir };
    let var_str = if (var >> 6) != 0 { (31 - ((var >> 6) as u32).leading_zeros()).min(12) as i32 } else { 0 };
    pri = if var != 0 { (pri * (4 + var_str) + 8) >> 4 } else { 0 };
    let damping = cp.damping as i32 + coeff_shift as i32;
    filter(fs, out, 0, r, c, pri, sec, damping, dir);
    if fs.num_planes == 1 {
        return true;
    }
    let pri = (cp.uv_pri[idx] << coeff_shift) as i32;
    let sec = (cp.uv_sec[idx] << coeff_shift) as i32;
    let dir = if pri == 0 { 0 } else { CDEF_UV_DIR[fs.ssx][fs.ssy][y_dir] as usize };
    let damping = cp.damping as i32 + coeff_shift as i32 - 1;
    filter(fs, out, 1, r, c, pri, sec, damping, dir);
    filter(fs, out, 2, r, c, pri, sec, damping, dir);
    true
}

fn direction(fs: &FrameState, r: usize, c: usize) -> (usize, i32) {
    let mut cost = [0i32; 8];
    let mut partial = [[0i32; 15]; 8];
    let x0 = c << 2;
    let y0 = r << 2;
    let pl = &fs.cur.planes[0];
    let sh = fs.bit_depth - 8;
    for i in 0..8 {
        let row = pl.row(y0 + i);
        for j in 0..8 {
            let x = (row[x0 + j] as i32 >> sh) - 128;
            partial[0][i + j] += x;
            partial[1][i + j / 2] += x;
            partial[2][i] += x;
            partial[3][3 + i - j / 2] += x;
            partial[4][7 + i - j] += x;
            partial[5][3 - i / 2 + j] += x;
            partial[6][j] += x;
            partial[7][i / 2 + j] += x;
        }
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
const CDEF_NA: i32 = i32::MIN;
/// Padded block: 8x8 plus 2 samples on every side, row stride 12.
const PAD_STRIDE: usize = 12;

#[inline(always)]
fn constrain(diff: i32, threshold: i32, adj: u32) -> i32 {
    let mag = (threshold - (diff.abs() >> adj)).clamp(0, diff.abs());
    if diff < 0 { -mag } else { mag }
}

#[allow(clippy::too_many_arguments)]
fn filter(fs: &FrameState, out: &mut [RowBand; 3], plane: usize, r: usize, c: usize, pri: i32, sec: i32, damping: i32, dir: usize) {
    if pri == 0 && sec == 0 {
        // Every tap contributes 0 and the clamp to [min, max] keeps x: CdefFrame = CurrFrame.
        return;
    }
    let coeff_shift = fs.bit_depth - 8;
    let (sub_x, sub_y) = if plane > 0 { (fs.ssx, fs.ssy) } else { (0, 0) };
    let x0 = (c * 4) >> sub_x;
    let y0 = (r * 4) >> sub_y;
    let w = 8 >> sub_x;
    let h = 8 >> sub_y;
    let src = &fs.cur.planes[plane];
    let (mi_rows, mi_cols) = (fs.fh.mi_rows as isize, fs.fh.mi_cols as isize);
    // CdefAvailable per sample: its 4x4 block is inside the frame.
    let mut buf = [CDEF_NA; PAD_STRIDE * PAD_STRIDE];
    let mut all_avail = true;
    for i in 0..h + 4 {
        let y = (y0 + i) as isize - 2;
        let cand_r = (y << sub_y) >> 2;
        if y < 0 || cand_r >= mi_rows {
            all_avail = false;
            continue;
        }
        let row = src.row(y as usize);
        for j in 0..w + 4 {
            let x = (x0 + j) as isize - 2;
            let cand_c = (x << sub_x) >> 2;
            if x >= 0 && cand_c < mi_cols {
                buf[i * PAD_STRIDE + j] = row[x as usize] as i32;
            } else {
                all_avail = false;
            }
        }
    }
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
    if all_avail {
        // Every tap is available: whole rows at a time (constrain() with threshold 0 is 0).
        let mut taps = [(0isize, 0i32, 0i32, 0u32); 12];
        let mut n = 0;
        for k in 0..2 {
            for sign in [-1isize, 1] {
                taps[n] = (sign * pri_off[k], pri_taps[k], pri, pri_adj);
                taps[n + 1] = (sign * sec_off[k][0], sec_taps[k], sec, sec_adj);
                taps[n + 2] = (sign * sec_off[k][1], sec_taps[k], sec, sec_adj);
                n += 3;
            }
        }
        let rows = (y0, x0, h);
        match w {
            8 => filter_rows::<8>(&buf, &taps, dst, rows),
            _ => filter_rows::<4>(&buf, &taps, dst, rows),
        }
        return;
    }
    for i in 0..h {
        // The centre sample is read even where CdefAvailable is 0 (8x8 blocks straddling an odd
        // MiRows / MiCols edge).
        let srow = &src.row(y0 + i)[x0..x0 + w];
        let drow = &mut dst.row_mut(y0 + i)[x0..x0 + w];
        for j in 0..w {
            let ci = ((i + 2) * PAD_STRIDE + j + 2) as isize;
            let x = srow[j] as i32;
            let mut sum = 0i32;
            let mut max = x;
            let mut min = x;
            for k in 0..2 {
                for sign in [-1isize, 1] {
                    let p = buf[(ci + sign * pri_off[k]) as usize];
                    if p != CDEF_NA {
                        if pri != 0 {
                            sum += pri_taps[k] * constrain(p - x, pri, pri_adj);
                        }
                        max = max.max(p);
                        min = min.min(p);
                    }
                    for s in 0..2 {
                        let q = buf[(ci + sign * sec_off[k][s]) as usize];
                        if q != CDEF_NA {
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

/// CDEF of a block whose taps are all available, `W` samples per row. Samples (at most 12
/// bits), differences and the weighted tap sum all fit in i16, which doubles the SIMD width.
#[inline(always)]
fn filter_rows<const W: usize>(
    buf: &[i32; PAD_STRIDE * PAD_STRIDE],
    taps: &[(isize, i32, i32, u32); 12],
    dst: &mut RowBand,
    (y0, x0, h): (usize, usize, usize),
) {
    let mut b16 = [0i16; PAD_STRIDE * PAD_STRIDE];
    for (d, &s) in b16.iter_mut().zip(buf.iter()) {
        *d = s as i16;
    }
    for i in 0..h {
        let ci = ((i + 2) * PAD_STRIDE + 2) as isize;
        let Some(&x) = b16.get(ci as usize..).and_then(|r| r.first_chunk::<W>()) else {
            return;
        };
        let (mut sum, mut max, mut min) = ([0i16; W], x, x);
        for &(off, weight, thr, adj) in taps {
            let (weight, thr) = (weight as i16, thr as i16);
            let start = (ci + off) as usize;
            let Some(p) = b16.get(start..).and_then(|r| r.first_chunk::<W>()) else {
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
