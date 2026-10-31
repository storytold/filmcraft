//! Loop filter process (8.8).

use crate::frame::{MiGrid, MiInfo};
use crate::header::{LoopFilterParams, Segmentation};
use crate::tables::{
    BLOCK_8X8, BLOCK_16X16, BLOCK_64X64, INTRA_FRAME, MAX_LOOP_FILTER, MAX_TXSIZE, NEARESTMV, NEARMV, NEWMV, NUM_8X8_HIGH, NUM_8X8_WIDE, SEG_LVL_ALT_L,
    SS_SIZE_LOOKUP, TX_4X4, TX_8X8, TX_16X16,
};
use filmcraft_simd::{LfSize, LfThresh};

/// Frame-level loop filter setup (8.8.1) plus the per-level limits of 8.8.4.
pub struct LfFrame {
    /// LvlLookup[segment_id][ref][modeType].
    pub lvl: [[[u8; 2]; 4]; 8],
    /// (limit, blimit, thresh) per filter level.
    pub limits: [(i32, i32, i32); 64],
    /// The limits per filter level for `filmcraft_simd`.
    pub thresh: [LfThresh; 64],
    pub mi_rows: usize,
    pub mi_cols: usize,
    pub ss_x: usize,
    pub ss_y: usize,
    pub bit_depth: u8,
    /// Filter with `filmcraft_simd`'s vector kernels; without (the scalar level), with the
    /// group filter below, which the compiler vectorizes for any target.
    pub vector: bool,
}

impl LfFrame {
    pub fn new(lf: &LoopFilterParams, seg: &Segmentation, mi_rows: usize, mi_cols: usize, ss_x: bool, ss_y: bool, bit_depth: u8) -> LfFrame {
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
        let vector = filmcraft_simd::level() != filmcraft_simd::Level::Scalar;
        let thresh = std::array::from_fn(|l| LfThresh::new(limits[l].0, limits[l].1, limits[l].2, bit_depth as u32));
        LfFrame { lvl, limits, thresh, mi_rows, mi_cols, ss_x: ss_x as usize, ss_y: ss_y as usize, bit_depth, vector }
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
        filter_sb_plane(view, mi, f, plane, row, col);
    }
}

/// The edges of one plane of a superblock (8.8.2), found once per 8x8 block of the plane.
///
/// Pass 0 filters the vertical edges, pass 1 the horizontal ones. Bit `edge * 8 + g` of a pass's
/// mask is the group of 8 samples `g` along its `edge`-th 8x8 block edge (blocks across, `g`
/// blocks along), so each edge is one byte, in filtering order. The edges 4 samples into a
/// block (between 4x4 transforms) always take the 4-tap filter.
struct SbEdges {
    /// [pass][TX_4X4, TX_8X8, TX_16X16]: the block edges filtered with that size.
    edge8: [[u64; 3]; 2],
    /// [pass]: the 4x4 transform edges inside the blocks.
    edge4: [u64; 2],
    /// Filter level of block (bx, by) at `by * 8 + bx`.
    lvl: [u8; 64],
    /// [pass][block along]: lines of the group inside the frame (1 to 8).
    lines: [[u8; 8]; 2],
}

impl SbEdges {
    /// The edges of `plane` in the superblock at (`row`, `col`) (MI units): the per-group
    /// decisions of 8.8.2. Every MI of a block holds the block's mode info, so the decisions
    /// are taken once per block, from its top left 8x8 block in the plane: those that depend
    /// on the position across an edge once per edge, and the block's groups along it set at once.
    fn new(mi: &MiGrid, f: &LfFrame, plane: usize, row: usize, col: usize) -> SbEdges {
        let (sub_x, sub_y) = if plane > 0 { (f.ss_x, f.ss_y) } else { (0, 0) };
        let (mi_rows, mi_cols) = (f.mi_rows, f.mi_cols);
        let mut e = SbEdges { edge8: [[0; 3]; 2], edge4: [0; 2], lvl: [0; 64], lines: [[0; 8]; 2] };
        for bx in 0..8 >> sub_x {
            let mc = col + (bx << sub_x);
            if mc < mi_cols {
                e.lines[1][bx] = ((8 * (mi_cols - mc)) >> sub_x).min(8) as u8;
            }
        }
        for by in 0..8 >> sub_y {
            let mr = row + (by << sub_y);
            if mr < mi_rows {
                e.lines[0][by] = ((8 * (mi_rows - mr)) >> sub_y).min(8) as u8;
            }
        }
        // The 8x8 blocks of the plane inside the frame.
        let nbx = ((mi_cols.saturating_sub(col) + (1 << sub_x) - 1) >> sub_x).min(8 >> sub_x);
        let nby = ((mi_rows.saturating_sub(row) + (1 << sub_y) - 1) >> sub_y).min(8 >> sub_y);
        // On a horizontal edge in the last odd chroma column (8.8.2) the 4x4 edges are not
        // filtered: the bit of that column, if it is in this superblock.
        let odd_last = match mi_cols.checked_sub(col + 1) {
            Some(last) if sub_x == 1 && last % 2 == 0 && last / 2 < 8 => 1u64 << (last / 2),
            _ => 0,
        };
        // The 8x8 blocks already covered by a block met earlier, bit by * 8 + bx.
        let mut done = 0u64;
        for by in 0..nby {
            for bx in 0..nbx {
                if done >> (by * 8 + bx) & 1 == 1 {
                    continue;
                }
                let (mr, mc) = (row + (by << sub_y), col + (bx << sub_x));
                let m = mi.at(mr, mc);
                let bs = m.sb_size.min(BLOCK_64X64) as usize;
                // The block's 8x8 blocks in the plane from here (it starts here: blocks are
                // aligned to their size and met in raster order).
                let w = ((NUM_8X8_WIDE[bs] as usize) >> sub_x).clamp(1, nbx - bx);
                let h = ((NUM_8X8_HIGH[bs] as usize) >> sub_y).clamp(1, nby - by);
                let row_bits = ((1u64 << w) - 1) << bx;
                for y in by..by + h {
                    done |= row_bits << (y * 8);
                }
                let lvl = f.level(m);
                if lvl == 0 {
                    continue;
                }
                for y in by..by + h {
                    e.lvl[y * 8 + bx..y * 8 + bx + w].fill(lvl);
                }
                let tx_sz = if plane > 0 {
                    if m.sb_size < BLOCK_8X8 { 0 } else { m.tx_size.min(MAX_TXSIZE[SS_SIZE_LOOKUP[bs][f.ss_x][f.ss_y].min(12) as usize]) }
                } else {
                    m.tx_size
                };
                let tx_edges = m.ref_frame[0] <= INTRA_FRAME || !m.skip;
                // Per pass: (the block's first 8x8 block across and its count, its groups along
                // the edges as bits; the superblock's first MI across, MI across in the frame,
                // subsampling across, blocks of the prediction block across).
                for (pass, (b0, n, along, m0, m_n, sub, wide)) in
                    [(bx, w, ((1u64 << h) - 1) << by, col, mi_cols, sub_x, &NUM_8X8_WIDE), (by, h, row_bits, row, mi_rows, sub_y, &NUM_8X8_HIGH)]
                        .into_iter()
                        .enumerate()
                {
                    let sb_size = if sub == 0 { m.sb_size } else { m.sb_size.max(BLOCK_16X16) } as usize;
                    for b in b0..b0 + n {
                        let m_at = m0 + (b << sub);
                        let bits = along << (b * 8);
                        // The block edge (not on the frame's left or top edge): its offset in luma
                        // samples is 8 * m_at, so it is a prediction block edge where m_at is a
                        // multiple of the block's 8x8 count, a transform edge where 2 * b (its
                        // index in 4-sample edges) is one of the transform's.
                        if m_at > 0 {
                            let block_edge = m_at & (wide[sb_size] as usize - 1) == 0;
                            let tx_ok = tx_edges && (2 * b) & ((1 << tx_sz) - 1) == 0;
                            if block_edge || tx_ok {
                                let base = if tx_sz == TX_4X4 && b % 4 == 0 { TX_8X8 } else { tx_sz.min(TX_16X16) };
                                let size = if base == TX_16X16 && sub == 1 && m_at == m_n - 1 { TX_8X8 } else { base };
                                e.edge8[pass][size as usize] |= bits;
                            }
                        }
                        // The edge 4 samples in: never a prediction block edge, a 4x4 transform
                        // edge if inside the frame (subsampled, that is 8 luma samples in: the next MI).
                        let inside = sub == 0 || m_at + 1 < m_n;
                        if tx_edges && tx_sz == TX_4X4 && inside {
                            let skip = if pass == 1 { odd_last } else { 0 };
                            e.edge4[pass] |= (along & !skip) << (b * 8);
                        }
                    }
                }
            }
        }
        e
    }
}

/// Loop filter one plane of a superblock in one pass (8.8.2): the edges across (left to right
/// or top to bottom), each group of 8 samples along it with its block's level and filter size.
fn filter_sb_plane(v: &mut PlaneView, mi: &MiGrid, f: &LfFrame, plane: usize, row: usize, col: usize) {
    let e = SbEdges::new(mi, f, plane, row, col);
    let (sub_x, sub_y) = if plane > 0 { (f.ss_x, f.ss_y) } else { (0, 0) };
    // The superblock's top left sample in the view.
    let (x0, y0) = ((col * 8) >> sub_x, (row * 8) >> sub_y);
    for pass in 0..2 {
        let [m4, m8, m16] = e.edge8[pass];
        let all = m4 | m8 | m16 | e.edge4[pass];
        if all == 0 {
            continue;
        }
        let blocks = 8 >> if pass == 0 { sub_x } else { sub_y };
        for b in 0..blocks {
            let sh = 8 * b;
            for (half, mut groups) in [(0, ((m4 | m8 | m16) >> sh) as u8), (1, (e.edge4[pass] >> sh) as u8)] {
                while groups != 0 {
                    let g = groups.trailing_zeros() as usize;
                    groups &= groups - 1;
                    let bit = 1u64 << (sh + g);
                    let size = if half == 1 || m4 & bit != 0 {
                        TX_4X4
                    } else if m8 & bit != 0 {
                        TX_8X8
                    } else {
                        TX_16X16
                    };
                    let (bx, by) = if pass == 0 { (b, g) } else { (g, b) };
                    let lvl = e.lvl[by * 8 + bx];
                    let across = 8 * b + 4 * half;
                    let (px, py) = if pass == 0 { (x0 + across, y0 + 8 * g) } else { (x0 + 8 * g, y0 + across) };
                    let pos = (py - v.oy) * v.stride + px - v.ox;
                    filter_group(v, pos, pass, e.lines[pass][g] as usize, size, lvl, f);
                }
            }
        }
    }
}

/// Filter the group of `n` lines from q0 at `pos` with the given size and level.
fn filter_group(v: &mut PlaneView, pos: usize, pass: usize, n: usize, filter_size: u8, lvl: u8, f: &LfFrame) {
    let on = ((1u16 << n) - 1) as u8;
    if f.vector {
        filmcraft_simd::vp9_deblock(v.data, pos, v.stride, pass == 0, (n, on), (lf_size(filter_size), &f.thresh[lvl as usize]));
        return;
    }
    let (limit, blimit, thresh) = f.limits[lvl as usize];
    let apply = std::array::from_fn(|l| l < n);
    let lim = Limits { apply, limit, blimit, thresh, filter_size, bit_depth: f.bit_depth };
    let (along, st) = if pass == 0 { (v.stride, 1) } else { (1, v.stride) };
    let d = &mut *v.data;
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

/// Samples along an edge filtered together.
const LANES: usize = 8;

/// The VP9 sample filter of a filter size.
fn lf_size(filter_size: u8) -> LfSize {
    match filter_size {
        TX_4X4 => LfSize::Vp9Four,
        TX_8X8 => LfSize::Vp9Eight,
        _ => LfSize::Vp9Sixteen,
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The loop filter driver before the edge masks: every group of 8 samples of every edge,
    /// its decisions taken from its MI.
    fn reference_sb_plane(v: &mut PlaneView, mi: &MiGrid, f: &LfFrame, plane: usize, pass: usize, row: usize, col: usize) {
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
                let is_block_edge =
                    if pass == 0 { x & (8 * NUM_8X8_WIDE[sb_size] as usize - 1) == 0 } else { y & (8 * NUM_8X8_HIGH[sb_size] as usize - 1) == 0 };
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
                let filter_size = if base_size == TX_16X16
                    && ((pass == 0 && sub_x == 1 && (x >> 3) == mi_cols - 1) || (pass == 1 && sub_y == 1 && (y >> 3) == mi_rows - 1))
                {
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
                    if f.vector {
                        reference_group_simd(v.data, pos, v.stride, pass == 0, n, &lim);
                        i += 8;
                        continue;
                    }
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

    /// The group through the vector kernels (the driver before the edge masks).
    fn reference_group_simd(d: &mut [u16], pos: usize, stride: usize, vertical: bool, n: usize, g: &Limits) {
        let on = g.apply.iter().enumerate().fold(0u8, |m, (l, &a)| m | ((a as u8) << l));
        let t = LfThresh::new(g.limit, g.blimit, g.thresh, g.bit_depth as u32);
        filmcraft_simd::vp9_deblock(d, pos, stride, vertical, (n, on), (lf_size(g.filter_size), &t));
    }

    /// The vector kernels filter every group as the group filter does: all filter sizes, bit
    /// depths, edge directions and lane masks, on smooth sides (the flat filters) and steps.
    #[test]
    fn vector_kernels_match_the_group_filter() {
        let mut seed = 0x1f00_d5edu32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let stride = 40usize;
        let mut changed = 0;
        for case in 0..30_000 {
            let bit_depth = [8u8, 10, 12][case % 3];
            let max = (1i32 << bit_depth) - 1;
            let u = 1i32 << (bit_depth - 8);
            let level = (next() % (max as u32 + 1)) as i32;
            let step = [0, 1, 2, 5, 30, 90][next() as usize % 6] * u;
            let noise = [0, 1, 2, 8][next() as usize % 4] * u;
            let pass = case / 3 % 2;
            let data: Vec<u16> = (0..stride * stride)
                .map(|i| {
                    let across = if pass == 0 { i % stride } else { i / stride };
                    let n = if noise == 0 { 0 } else { (next() % (noise as u32 + 1)) as i32 - noise / 2 };
                    (level + if across >= 16 { step } else { 0 } + n).clamp(0, max) as u16
                })
                .collect();
            let lvl = 1 + (next() % 63) as i32;
            let limit = (lvl >> (next() % 2)).max(1);
            let mut apply: [bool; LANES] = if next() % 3 == 0 { std::array::from_fn(|_| next() % 4 != 0) } else { [true; LANES] };
            let n = if next() % 4 == 0 { 4 } else { 8 };
            apply[n..].fill(false);
            let filter_size = [TX_4X4, TX_8X8, TX_16X16][next() as usize % 3];
            let g = Limits { apply, limit, blimit: 2 * (lvl + 2) + limit, thresh: lvl >> 4, filter_size, bit_depth };
            // q0 of the first line at (16, 8) for a vertical edge, (8, 16) for a horizontal one.
            let pos = if pass == 0 { 8 * stride + 16 } else { 16 * stride + 8 };
            let (along, across) = if pass == 0 { (stride, 1) } else { (1, stride) };
            let mut want = data.clone();
            match (bit_depth <= 10, filter_size == TX_16X16, pass == 1) {
                (true, false, false) => lanes16::filter_group::<4, false>(&mut want, pos, along, across, &g),
                (true, false, true) => lanes16::filter_group::<4, true>(&mut want, pos, along, across, &g),
                (true, true, false) => lanes16::filter_group::<8, false>(&mut want, pos, along, across, &g),
                (true, true, true) => lanes16::filter_group::<8, true>(&mut want, pos, along, across, &g),
                (false, false, false) => lanes32::filter_group::<4, false>(&mut want, pos, along, across, &g),
                (false, false, true) => lanes32::filter_group::<4, true>(&mut want, pos, along, across, &g),
                (false, true, false) => lanes32::filter_group::<8, false>(&mut want, pos, along, across, &g),
                (false, true, true) => lanes32::filter_group::<8, true>(&mut want, pos, along, across, &g),
            }
            changed += (want != data) as usize;
            let on = apply.iter().enumerate().fold(0u8, |m, (l, &a)| m | ((a as u8) << l));
            let t = LfThresh::new(g.limit, g.blimit, g.thresh, bit_depth as u32);
            for &level in filmcraft_simd::levels() {
                let mut got = data.clone();
                filmcraft_simd::vp9_deblock_at(level, &mut got, pos, stride, pass == 0, (n, on), (lf_size(filter_size), &t));
                assert_eq!(got, want, "{level:?} case {case} bd {bit_depth} size {filter_size} pass {pass} n {n} apply {apply:?}");
            }
        }
        assert!(changed > 10_000, "{changed}");
    }

    /// Partition the `n` x `n` MI square at (`r`, `c`) at random (6.4.3) into blocks with
    /// random mode info.
    fn random_partition(mi: &mut MiGrid, r: usize, c: usize, n: usize, next: &mut dyn FnMut() -> u32) {
        if r >= mi.rows || c >= mi.cols {
            return;
        }
        let kind = next() % 4;
        if n == 1 {
            // 8x8, 8x4, 4x8 and 4x4 blocks are all one MI.
            return put_block(mi, r, c, 1, 1, [BLOCK_8X8, 2, 1, 0][kind as usize], next);
        }
        // The square block size and its HORZ (one smaller) and VERT (two smaller) halves.
        let sq = match n {
            8 => BLOCK_64X64,
            4 => 9,
            _ => BLOCK_16X16,
        };
        let h = n / 2;
        match kind {
            0 => put_block(mi, r, c, n, n, sq, next),
            1 => {
                put_block(mi, r, c, n, h, sq - 1, next);
                put_block(mi, r + h, c, n, h, sq - 1, next);
            }
            2 => {
                put_block(mi, r, c, h, n, sq - 2, next);
                put_block(mi, r, c + h, h, n, sq - 2, next);
            }
            _ => {
                for (dr, dc) in [(0, 0), (0, h), (h, 0), (h, h)] {
                    random_partition(mi, r + dr, c + dc, h, next);
                }
            }
        }
    }

    /// A `w` x `h` (MI) block of size `sb_size` at (`r`, `c`) with random mode info.
    fn put_block(mi: &mut MiGrid, r: usize, c: usize, w: usize, h: usize, sb_size: u8, next: &mut dyn FnMut() -> u32) {
        let m = MiInfo {
            sb_size,
            skip: next().is_multiple_of(2),
            tx_size: (next() % 4) as u8,
            y_mode: (next() % 14) as u8,
            seg_id: (next() % 8) as u8,
            ref_frame: [(next() % 4) as i8, -1],
            ..MiInfo::default()
        };
        for y in r..(r + h).min(mi.rows) {
            for x in c..(c + w).min(mi.cols) {
                mi.mi[y * mi.cols + x] = m;
            }
        }
    }

    /// The edge masks filter every frame as the per-group driver does: random partitions with random
    /// mode info (block and transform sizes, skip, references, levels with zeros), frame sizes with odd MI counts,
    /// every subsampling and bit depth, with the vector kernels and the group filter.
    #[test]
    fn edge_masks_filter_like_the_group_driver() {
        let mut seed = 0x5eed_0b17u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut changed = 0;
        for case in 0..240 {
            let (ss_x, ss_y) = [(1, 1), (1, 0), (0, 1), (0, 0)][case % 4];
            let bit_depth = [8u8, 10, 12][case / 4 % 3];
            let vector = case / 12 % 2 == 0;
            let mi_cols = 1 + next() as usize % 21;
            let mi_rows = 1 + next() as usize % 21;
            // Random partitions of every superblock into blocks of random mode info, each
            // block's MI the same (as decoding writes them), clipped by the frame's edges.
            let mut mi = MiGrid { cols: mi_cols, rows: mi_rows, mi: vec![MiInfo::default(); mi_rows * mi_cols] };
            for r in (0..mi_rows).step_by(8) {
                for c in (0..mi_cols).step_by(8) {
                    random_partition(&mut mi, r, c, 8, &mut next);
                }
            }
            let lvl = std::array::from_fn(|_| std::array::from_fn(|_| std::array::from_fn(|_| if next() % 5 == 0 { 0 } else { (next() % 64) as u8 })));
            let limits: [(i32, i32, i32); 64] = std::array::from_fn(|l| {
                let l = l as i32;
                (l.max(1), 2 * (l + 2) + l.max(1), l >> 4)
            });
            let thresh = std::array::from_fn(|l| LfThresh::new(limits[l].0, limits[l].1, limits[l].2, bit_depth as u32));
            let f = LfFrame { lvl, limits, thresh, mi_rows, mi_cols, ss_x, ss_y, bit_depth, vector };
            let max = (1u32 << bit_depth) - 1;
            // Planes padded to whole superblocks, smooth with steps and noise so edges filter.
            let planes: Vec<(Vec<u16>, usize)> = (0..3)
                .map(|p| {
                    let (sx, sy) = if p > 0 { (ss_x, ss_y) } else { (0, 0) };
                    let (w, h) = ((mi_cols.div_ceil(8) * 64) >> sx, (mi_rows.div_ceil(8) * 64) >> sy);
                    let base = next() % (max + 1);
                    let d = (0..w * h)
                        .map(|i| {
                            let step = if (i % w / 4 + i / w / 4) % 3 == 0 { max / 16 } else { 0 };
                            ((base + step + next() % 4) % (max + 1)) as u16
                        })
                        .collect();
                    (d, w)
                })
                .collect();
            let mut want = planes.clone();
            let mut got = planes.clone();
            for r in 0..mi_rows.div_ceil(8) {
                for c in 0..mi_cols.div_ceil(8) {
                    for (plane, (d, w)) in want.iter_mut().enumerate() {
                        let mut v = PlaneView { data: d, stride: *w, ox: 0, oy: 0 };
                        for pass in 0..2 {
                            reference_sb_plane(&mut v, &mi, &f, plane, pass, r * 8, c * 8);
                        }
                    }
                    let [a, b, d] = &mut got[..] else { unreachable!() };
                    let mut views = [a, b, d].map(|(d, w)| PlaneView { data: d, stride: *w, ox: 0, oy: 0 });
                    filter_superblock(&mut views, &mi, &f, r * 8, c * 8);
                }
            }
            changed += (want != planes) as usize;
            assert!(got == want, "case {case}: {mi_cols}x{mi_rows} ss {ss_x},{ss_y} bd {bit_depth} vector {vector}");
        }
        assert!(changed > 200, "{changed}");
    }
}
