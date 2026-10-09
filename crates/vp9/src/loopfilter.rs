//! Loop filter process (8.8).

use crate::frame::{MiGrid, MiInfo};
use crate::header::{LoopFilterParams, Segmentation};
use crate::tables::*;

/// Frame-level loop filter setup (8.8.1) plus the per-level limits of 8.8.4.
pub struct LfFrame {
    /// LvlLookup[segment_id][ref][modeType].
    pub lvl: [[[u8; 2]; 4]; 8],
    /// (limit, blimit, thresh) per filter level.
    pub limits: [(i32, i32, i32); 64],
    pub mi_rows: usize,
    pub mi_cols: usize,
    pub ss_x: usize,
    pub ss_y: usize,
    pub bit_depth: u8,
}

/// Loop filter levels (8.8.1): `[segment][reference frame (intra, last, golden, altref)][mode
/// (ZEROMV, other)]`.
pub(crate) fn segment_filter_levels(lf: &LoopFilterParams, seg: &Segmentation) -> [[[u8; 2]; 4]; 8] {
    let mut lvl = [[[0u8; 2]; 4]; 8];
    let n_shift = lf.level >> 5;
    for (seg_id, l) in lvl.iter_mut().enumerate() {
        let mut lvl_seg = lf.level as i32;
        if seg.feature_active(seg_id as u8, SEG_LVL_ALT_L) {
            let d = seg.feature_data[seg_id][SEG_LVL_ALT_L] as i32;
            lvl_seg = if seg.abs_or_delta_update { d } else { d + lf.level as i32 };
            lvl_seg = lvl_seg.clamp(0, MAX_LOOP_FILTER);
        }
        if !lf.delta_enabled {
            *l = [[lvl_seg as u8; 2]; 4];
        } else {
            let intra = lvl_seg + ((lf.ref_deltas[0] as i32) << n_shift);
            l[0][0] = intra.clamp(0, MAX_LOOP_FILTER) as u8;
            l[0][1] = l[0][0];
            for rf in 1..4 {
                for mode in 0..2 {
                    let v = lvl_seg + ((lf.ref_deltas[rf] as i32) << n_shift) + ((lf.mode_deltas[mode] as i32) << n_shift);
                    l[rf][mode] = v.clamp(0, MAX_LOOP_FILTER) as u8;
                }
            }
        }
    }
    lvl
}

impl LfFrame {
    pub fn new(lf: &LoopFilterParams, seg: &Segmentation, mi_rows: usize, mi_cols: usize, ss_x: bool, ss_y: bool, bit_depth: u8) -> LfFrame {
        let lvl = segment_filter_levels(lf, seg);
        let mut limits = [(0, 0, 0); 64];
        let sh = lf.sharpness as i32;
        let shift = if sh > 4 {
            2
        } else if sh > 0 {
            1
        } else {
            0
        };
        for (l, e) in limits.iter_mut().enumerate() {
            let l = l as i32;
            let limit = if sh > 0 { (l >> shift).clamp(1, 9 - sh) } else { (l >> shift).max(1) };
            *e = (limit, 2 * (l + 2) + limit, l >> 4);
        }
        LfFrame { lvl, limits, mi_rows, mi_cols, ss_x: ss_x as usize, ss_y: ss_y as usize, bit_depth }
    }

    fn level(&self, mi: &MiInfo) -> u8 {
        let mode_type = matches!(mi.y_mode, NEARESTMV | NEARMV | NEWMV) as usize;
        let rf = mi.ref_frame[0].max(0) as usize;
        self.lvl[mi.seg_id as usize & 7][rf][mode_type]
    }
}

/// A mutable view of one plane: sample (x, y) of the plane is `data[(y - oy) * stride + x - ox]`.
pub struct PlaneView<'a> {
    pub data: &'a mut [u16],
    pub stride: usize,
    pub ox: usize,
    pub oy: usize,
}

/// Loop filter one superblock (all planes, both passes) at (`row`, `col`) in MI units (8.8.2).
pub fn filter_superblock(views: &mut [PlaneView; 3], mi: &MiGrid, f: &LfFrame, row: usize, col: usize) {
    for (plane, view) in views.iter_mut().enumerate() {
        for pass in 0..2 {
            filter_sb_plane(view, mi, f, plane, pass, row, col);
        }
    }
}

fn filter_sb_plane(v: &mut PlaneView, mi: &MiGrid, f: &LfFrame, plane: usize, pass: usize, row: usize, col: usize) {
    let (sub_x, sub_y) = if plane > 0 { (f.ss_x, f.ss_y) } else { (0, 0) };
    let (sub, edge_len) = if pass == 0 { (sub_x, 64 >> sub_y) } else { (sub_y, 64 >> sub_x) };
    let (mi_rows, mi_cols) = (f.mi_rows, f.mi_cols);
    // Step between the samples across the edge.
    let across = if pass == 0 { 1isize } else { v.stride as isize };
    for edge in 0..(16 >> sub) {
        let mut i = 0;
        while i < edge_len {
            // Group of 8 samples sharing loopRow / loopCol.
            let (x, y) =
                if pass == 0 { (col * 8 + edge * (4 << sub_x), row * 8 + (i << sub_y)) } else { (col * 8 + (i << sub_x), row * 8 + edge * (4 << sub_y)) };
            // Skip groups entirely off screen (the varying coordinate is re-checked per sample).
            if x >= 8 * mi_cols || y >= 8 * mi_rows || (pass == 0 && x == 0) || (pass == 1 && y == 0) {
                i += 8;
                continue;
            }
            let loop_col = ((x >> 3) >> sub_x) << sub_x;
            let loop_row = ((y >> 3) >> sub_y) << sub_y;
            let m = mi.at(loop_row, loop_col);
            let tx_sz = if plane > 0 {
                if m.sb_size < BLOCK_8X8 { 0 } else { m.tx_size.min(MAX_TXSIZE[SS_SIZE_LOOKUP[m.sb_size as usize][f.ss_x][f.ss_y].min(12) as usize]) }
            } else {
                m.tx_size
            };
            let sb_size = if sub == 0 { m.sb_size } else { m.sb_size.max(BLOCK_16X16) } as usize;
            let is_intra = m.ref_frame[0] <= INTRA_FRAME;
            // Block sizes are powers of two, so the modulo is a mask.
            let is_block_edge = if pass == 0 { x & (8 * NUM_8X8_WIDE[sb_size] as usize - 1) == 0 } else { y & (8 * NUM_8X8_HIGH[sb_size] as usize - 1) == 0 };
            let tx_ok = edge & ((1 << tx_sz) - 1) == 0 && (is_intra || !m.skip);
            if !is_block_edge && !tx_ok {
                i += 8;
                continue;
            }
            let lvl = f.level(m);
            if lvl == 0 {
                i += 8;
                continue;
            }
            let is32 = edge % 8 == 0;
            let base_size = if tx_sz == TX_4X4 && is32 { TX_8X8 } else { tx_sz.min(TX_16X16) };
            let filter_size =
                if base_size == TX_16X16 && ((pass == 0 && sub_x == 1 && (x >> 3) == mi_cols - 1) || (pass == 1 && sub_y == 1 && (y >> 3) == mi_rows - 1)) {
                    TX_8X8
                } else {
                    base_size
                };
            let (limit, blimit, thresh) = f.limits[lvl as usize];
            // Lanes of the group (the 8 samples along the edge) inside the MI grid.
            let n = if pass == 0 { (8 * mi_rows - y).div_ceil(1 << sub_y) } else { (8 * mi_cols - x).div_ceil(1 << sub_x) }.min(LANES).min(edge_len - i);
            // A transform edge is not filtered in the last odd chroma column (8.8.2).
            let odd_last = pass == 1 && sub_x == 1 && mi_cols & 1 == 1 && edge & 1 == 1;
            let mut apply = [false; LANES];
            for (l, a) in apply.iter_mut().enumerate().take(n) {
                let is_tx_edge = tx_ok && !(odd_last && x + (l << sub_x) + 8 >= mi_cols * 8);
                *a = is_block_edge || is_tx_edge;
            }
            if apply.iter().any(|&a| a) {
                let px = x >> sub_x;
                let py = y >> sub_y;
                let pos = (py - v.oy) * v.stride + px - v.ox;
                let along = if pass == 0 { v.stride } else { 1 };
                let lim = Limits { apply, limit, blimit, thresh, filter_size, bit_depth: f.bit_depth };
                let (d, st) = (&mut *v.data, across as usize);
                match (f.bit_depth <= 10, filter_size == TX_16X16, pass == 1) {
                    (true, false, false) => lanes16::filter_group::<4, false>(d, pos, along, st, &lim),
                    (true, false, true) => lanes16::filter_group::<4, true>(d, pos, along, st, &lim),
                    (true, true, false) => lanes16::filter_group::<8, false>(d, pos, along, st, &lim),
                    (true, true, true) => lanes16::filter_group::<8, true>(d, pos, along, st, &lim),
                    (false, false, false) => lanes32::filter_group::<4, false>(d, pos, along, st, &lim),
                    (false, false, true) => lanes32::filter_group::<4, true>(d, pos, along, st, &lim),
                    (false, true, false) => lanes32::filter_group::<8, false>(d, pos, along, st, &lim),
                    (false, true, true) => lanes32::filter_group::<8, true>(d, pos, along, st, &lim),
                }
            }
            i += 8;
        }
    }
}

/// Samples along an edge filtered together.
const LANES: usize = 8;

/// Per-group filter parameters.
struct Limits {
    apply: [bool; LANES],
    limit: i32,
    blimit: i32,
    thresh: i32,
    filter_size: u8,
    bit_depth: u8,
}

/// The group filter, instantiated for 16-bit lanes (bit depths 8 and 10: every intermediate,
/// including the 16-tap sums of at most 16 * 1023 + 8, fits) and 32-bit lanes (12-bit).
macro_rules! group_filter {
    ($m:ident, $t:ty) => {
        mod $m {
            use super::{LANES, Limits};
            use crate::tables::*;
            type T = $t;

            /// Sample filtering process (8.8.5) for the up to 8 lanes of a group whose `apply` flag is set.
            /// Lane `l` has its q0 sample at `pos + l * along`; samples across the edge are `step` apart
            /// (`ROWS`: `along` is 1, i.e. a horizontal edge). `R` samples on each side are read (8 for
            /// 16x16 filters, else 4). Every lane is computed with every filter and the result chosen per
            /// lane (8.8.5.1 decides which), so the arithmetic is branch-free and vectorizes; unmodified
            /// samples are written back unchanged.
            #[inline(always)]
            pub(super) fn filter_group<const R: usize, const ROWS: bool>(d: &mut [u16], pos: usize, along: usize, step: usize, g: &Limits) {
                let Limits { apply, limit, blimit, thresh, filter_size, bit_depth } = *g;
                let (limit, blimit, thresh) = (limit as T, blimit as T, thresh as T);
                // v[8 + k][l] = sample at offset k across the edge in lane l.
                let mut v = [[0 as T; LANES]; 16];
                let start = pos - R * step;
                if ROWS {
                    for k in 0..2 * R {
                        let row: &[u16; LANES] = d[start + k * step..start + k * step + LANES].try_into().expect("lanes");
                        v[8 - R + k] = std::array::from_fn(|l| row[l] as T);
                    }
                } else {
                    for l in 0..LANES {
                        let s = start + l * along;
                        let line: &[u16] = &d[s..s + 2 * R];
                        for k in 0..2 * R {
                            v[8 - R + k][l] = line[k] as T;
                        }
                    }
                }
                let shift = bit_depth as u32 - 8;
                let limit_bd = limit << shift;
                let blimit_bd = blimit << shift;
                let thresh_bd = thresh << shift;
                let one = 1 << shift;
                let (p3, p2, p1, p0, q0, q1, q2, q3) = (&v[4], &v[5], &v[6], &v[7], &v[8], &v[9], &v[10], &v[11]);
                // Filter mask process (8.8.5.1). Masks are 0 / -1 per lane, selects are bitwise
                // and clipping uses min / max (`clamp` asserts its bounds), so every lane loop
                // below compiles to vector instructions.
                let le = |a: T, b: T| -((a <= b) as T);
                let sel = |m: T, a: T, b: T| (a & m) | (b & !m);
                let mut mask = [0 as T; LANES];
                let mut hev = [0 as T; LANES];
                let mut flat = [0 as T; LANES];
                let mut flat2 = [0 as T; LANES];
                let can_flat = -((filter_size >= TX_8X8) as T);
                let can_flat2 = R == 8 && filter_size >= TX_16X16;
                let apply: [T; LANES] = apply.map(|a| -(a as T));
                for l in 0..LANES {
                    let d_p1p0 = (p1[l] - p0[l]).abs();
                    let d_q1q0 = (q1[l] - q0[l]).abs();
                    mask[l] = apply[l]
                        & le((p3[l] - p2[l]).abs(), limit_bd)
                        & le((p2[l] - p1[l]).abs(), limit_bd)
                        & le(d_p1p0, limit_bd)
                        & le(d_q1q0, limit_bd)
                        & le((q2[l] - q1[l]).abs(), limit_bd)
                        & le((q3[l] - q2[l]).abs(), limit_bd)
                        & le((p0[l] - q0[l]).abs() * 2 + ((p1[l] - q1[l]).abs() >> 1), blimit_bd);
                    hev[l] = !(le(d_p1p0, thresh_bd) & le(d_q1q0, thresh_bd));
                    flat[l] = can_flat
                        & mask[l]
                        & le(d_p1p0, one)
                        & le(d_q1q0, one)
                        & le((p2[l] - p0[l]).abs(), one)
                        & le((q2[l] - q0[l]).abs(), one)
                        & le((p3[l] - p0[l]).abs(), one)
                        & le((q3[l] - q0[l]).abs(), one);
                }
                if mask.iter().fold(0, |a, &m| a | m) == 0 {
                    return;
                }
                if can_flat2 {
                    for l in 0..LANES {
                        flat2[l] = flat[l]
                            & le((v[0][l] - p0[l]).abs(), one)
                            & le((v[1][l] - p0[l]).abs(), one)
                            & le((v[2][l] - p0[l]).abs(), one)
                            & le((v[3][l] - p0[l]).abs(), one)
                            & le((v[12][l] - q0[l]).abs(), one)
                            & le((v[13][l] - q0[l]).abs(), one)
                            & le((v[14][l] - q0[l]).abs(), one)
                            & le((v[15][l] - q0[l]).abs(), one);
                    }
                }
                let any_flat = flat.iter().fold(0, |a, &f| a | f) != 0;
                let any_flat2 = flat2.iter().fold(0, |a, &f| a | f) != 0;
                // Results per position across the edge; start from the unfiltered samples.
                let mut out = v;
                // Narrow filter (8.8.5.2) for lanes that are masked but not flat.
                {
                    let lo = -(1 << (bit_depth - 1)) as T;
                    let hi = ((1 << (bit_depth - 1)) - 1) as T;
                    let c = |x: T| x.max(lo).min(hi);
                    let off: T = 0x80 << shift;
                    for l in 0..LANES {
                        let (ps1, ps0, qs0, qs1) = (p1[l] - off, p0[l] - off, q0[l] - off, q1[l] - off);
                        let filter = c((c(ps1 - qs1) & hev[l]) + 3 * (qs0 - ps0));
                        let filter1 = c(filter + 4) >> 3;
                        let filter2 = c(filter + 3) >> 3;
                        let f = (filter1 + 1) >> 1;
                        let narrow = mask[l] & !flat[l];
                        let outer = narrow & !hev[l];
                        out[8][l] = sel(narrow, c(qs0 - filter1) + off, out[8][l]);
                        out[7][l] = sel(narrow, c(ps0 + filter2) + off, out[7][l]);
                        out[9][l] = sel(outer, c(qs1 - f) + off, out[9][l]);
                        out[6][l] = sel(outer, c(ps1 + f) + off, out[6][l]);
                    }
                }
                // Wide filter (8.8.5.3) with 8 taps for flat lanes (p2..q2) ...
                if any_flat {
                    let w8 = wide_filter::<3>(&v);
                    for k in 5..11 {
                        for l in 0..LANES {
                            out[k][l] = sel(flat[l] & !flat2[l], w8[k][l], out[k][l]);
                        }
                    }
                }
                // ... and 16 taps for flat2 lanes (p6..q6).
                if any_flat2 {
                    let w16 = wide_filter::<4>(&v);
                    for k in 1..15 {
                        for l in 0..LANES {
                            out[k][l] = sel(flat2[l], w16[k][l], out[k][l]);
                        }
                    }
                }
                let (lo, hi) = if any_flat2 {
                    (1, 15)
                } else if any_flat {
                    (5, 11)
                } else {
                    (6, 10)
                };
                if ROWS {
                    for k in lo..hi {
                        let o = start + (k + R - 8) * step; // k >= 8 - R
                        let row: &mut [u16; LANES] = (&mut d[o..o + LANES]).try_into().expect("lanes");
                        for l in 0..LANES {
                            row[l] = out[k][l] as u16;
                        }
                    }
                } else {
                    // Whole lines: unmodified samples are rewritten with their own value.
                    for l in 0..LANES {
                        let s = start + l * along;
                        let line: &mut [u16] = &mut d[s..s + 2 * R];
                        for k in 0..2 * R {
                            line[k] = out[8 - R + k][l] as u16;
                        }
                    }
                }
            }

            /// Wide filter process (8.8.5.3) with 2^LOG2 taps on gathered lanes (positions 8 - 2^(LOG2-1)
            /// .. 8 + 2^(LOG2-1) - 2 are produced); the sum over j of the specification is computed as a
            /// sliding window over the clamped indices (identical results).
            #[inline(always)]
            fn wide_filter<const LOG2: u32>(v: &[[T; LANES]; 16]) -> [[T; LANES]; 16] {
                let n = (1isize << (LOG2 - 1)) - 1;
                let s = |k: isize| &v[(k.clamp(-(n + 1), n) + 8) as usize];
                let mut out = [[0 as T; LANES]; 16];
                let mut sum = [0 as T; LANES];
                for j in -n..=n {
                    let a = s(-n + j);
                    for l in 0..LANES {
                        sum[l] += a[l];
                    }
                }
                let round: T = 1 << (LOG2 - 1);
                for i in -n..n {
                    let (cur, add, sub) = (s(i), s(i + 1 + n), s(i - n));
                    let o = &mut out[(i + 8) as usize];
                    for l in 0..LANES {
                        o[l] = (sum[l] + cur[l] + round) >> LOG2;
                        sum[l] += add[l] - sub[l];
                    }
                }
                out
            }
        }
    };
}
group_filter!(lanes16, i16);
group_filter!(lanes32, i32);
