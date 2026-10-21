//! In-loop post filters run by decode_frame_wrapup: loop filter (7.14), CDEF (7.15),
//! super-resolution upscaling (7.16) and loop restoration (7.17).

use crate::frame::RowBand;
use crate::spec_tables::*;
use crate::state::FrameState;
use crate::stats::{DecodeStats, Stage, Timer};

/// Luma rows per band of the row-parallel filters: whole 64x64 CDEF filter blocks, and no
/// transform block crosses a band edge.
const BAND_ROWS: usize = 64;

/// The [start, end) luma rows of the bands covering `rows`: [`BAND_ROWS`] each (the last one
/// runs to the end of the plane), or one band for everything when `pool` is sequential.
pub(crate) fn bands(pool: &crate::par::Pool, rows: usize) -> Vec<(usize, usize)> {
    let n = if pool.is_parallel() { rows.div_ceil(BAND_ROWS).max(1) } else { 1 };
    (0..n).map(|b| (b * BAND_ROWS, if b + 1 == n { usize::MAX } else { (b + 1) * BAND_ROWS })).collect()
}

/// The post-filters run on the frame worker, each in row bands on `pool` (the result does not
/// depend on how the bands are scheduled). `draft`: only super-resolution (which sets the
/// output size) runs.
pub(crate) fn apply(fs: &mut FrameState, stats: &mut DecodeStats, pool: &crate::par::Pool, draft: bool) -> crate::Result<()> {
    let mut t = Timer::start();
    let lvl = fs.fh.lf.level;
    if (lvl[0] != 0 || lvl[1] != 0) && !draft {
        loop_filter(fs, pool);
    }
    stats.add(Stage::LoopFilter, t.lap());
    let cdef = if draft { None } else { crate::cdef::apply(fs, pool)? };
    stats.add(Stage::Cdef, t.lap());
    let (up_cur, up_cdef) = if fs.fh.use_superres {
        let c = crate::restoration::upscale(fs, &fs.cur);
        let d = cdef.as_ref().map(|f| crate::restoration::upscale(fs, f));
        (c, d)
    } else {
        (std::mem::take(&mut fs.cur), cdef)
    };
    stats.add(Stage::Superres, t.lap());
    fs.cur = if fs.fh.lr.uses_lr && !draft {
        crate::restoration::loop_restoration(fs, &up_cur, up_cdef.as_ref().unwrap_or(&up_cur), pool)?
    } else {
        up_cdef.unwrap_or(up_cur)
    };
    stats.add(Stage::Restoration, t.lap());
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Loop filter (7.14)

/// Luma rows above a band that its horizontal edges read or write (p6 .. p0 of the widest
/// filter, rounded up to whole 4x4 rows); chroma: `LF_MARGIN >> subsampling_y`.
const LF_MARGIN: usize = 8;

/// The loop filter in row bands, all of a frame's in one parallel pass (each join costs a
/// worker wake-up, which is slow on a busy machine).
///
/// Vertical edges (pass 0) only touch the 4 rows of their own edge. A horizontal edge (pass 1)
/// reads and writes at most `filter_size / 2` samples on each side, never more than the
/// transform blocks on either side are high, so no two edges touch the same sample and
/// any order gives the spec's raster-order result. Transform blocks never cross a band edge, so
/// the only edges reaching outside a band are those on its top row, whose p side lies in the
/// band above. Each band therefore also filters the vertical edges of a copy of the
/// `LF_MARGIN` rows above it (the same work as the band above does on them), runs its top
/// horizontal edges against that copy, and afterwards the samples those edges changed are
/// written back; the band above never touches them.
fn loop_filter(fs: &mut FrameState, pool: &crate::par::Pool) {
    let luma = bands(pool, fs.fh.mi_rows as usize * 4);
    let planes: Vec<usize> = (0..fs.num_planes).filter(|&p| p == 0 || fs.fh.lf.level[1 + p] != 0).collect();
    let mut cur = std::mem::take(&mut fs.cur);
    let f: &FrameState = fs;
    // Unfiltered copies of the rows above each band but the first.
    let margins: Vec<[Vec<u16>; 3]> = luma
        .iter()
        .map(|&(start, _)| {
            std::array::from_fn(|p| {
                let sub_y = if p == 0 { 0 } else { f.ssy };
                let pl = &cur.planes[p];
                let (y0, y1) = ((start.saturating_sub(LF_MARGIN)) >> sub_y, start >> sub_y);
                let rows = pl.data.get(y0 * pl.stride..y1 * pl.stride);
                if start == 0 || !planes.contains(&p) { Vec::new() } else { rows.map(<[u16]>::to_vec).unwrap_or_default() }
            })
        })
        .collect();
    let mut bands: Vec<([RowBand; 3], [Vec<u16>; 3])> = cur.row_bands(&luma).into_iter().zip(margins).collect();
    let changed = pool.map_mut(&mut bands, |b, (band, margin)| {
        let mut changed: [Vec<(usize, u16)>; 3] = Default::default();
        for &p in &planes {
            changed[p] = filter_band(f, p, luma[b], &mut band[p], &mut margin[p]);
        }
        changed
    });
    drop(bands);
    for (&(start, _), ch) in luma.iter().zip(&changed) {
        for (p, list) in ch.iter().enumerate() {
            let sub_y = if p == 0 { 0 } else { f.ssy };
            let pl = &mut cur.planes[p];
            let first = (start.saturating_sub(LF_MARGIN) >> sub_y) * pl.stride;
            for &(i, v) in list {
                if let Some(d) = pl.data.get_mut(first + i) {
                    *d = v;
                }
            }
        }
    }
    fs.cur = cur;
}

/// Both loop filter passes over the band of `plane` holding luma rows [start, end); `margin` is
/// an unfiltered copy of the rows above the band (empty for the first band). Returns the samples
/// of `margin` that the band's top horizontal edges changed, as (index, value).
fn filter_band(fs: &FrameState, plane: usize, (start, end): (usize, usize), band: &mut RowBand, margin: &mut [u16]) -> Vec<(usize, u16)> {
    let mi_rows = fs.fh.mi_rows as usize;
    let (r0, r1) = (start / 4, mi_rows.min(end / 4));
    let sub_y = if plane == 0 { 0 } else { fs.ssy };
    let edges = |band: &mut RowBand, mut above: Option<&mut RowBand>, pass: usize, rows: std::ops::Range<usize>| {
        let row_step = if plane == 0 { 1 } else { 1 << fs.ssy };
        let col_step = if plane == 0 { 1 } else { 1 << fs.ssx };
        let mi_cols = fs.fh.mi_cols as usize;
        let mut row = rows.start;
        while row < rows.end {
            let mut col = 0;
            while col < mi_cols {
                edge(fs, band, above.as_deref_mut(), plane, pass, row, col);
                col += col_step;
            }
            row += row_step;
        }
    };
    edges(band, None, 0, r0..r1);
    if margin.is_empty() {
        edges(band, None, 1, r0..r1);
        return Vec::new();
    }
    let stride = band.stride;
    let y0 = start.saturating_sub(LF_MARGIN) >> sub_y;
    let mut above = RowBand { data: margin, stride, y0 };
    edges(&mut above, None, 0, r0.saturating_sub(LF_MARGIN / 4)..r0);
    let before = above.data.to_vec();
    edges(band, Some(&mut above), 1, r0..r1);
    above.data.iter().zip(&before).enumerate().filter(|(_, (a, b))| a != b).map(|(i, (&a, _))| (i, a)).collect()
}

/// One loop filter edge (7.14.2) at mode info position (row, col): 4 lines of samples across
/// it. `above` holds the rows above `band` for horizontal edges on the band's top row.
fn edge(fs: &FrameState, band: &mut RowBand, above: Option<&mut RowBand>, plane: usize, pass: usize, row: usize, col: usize) {
    let (sub_x, sub_y) = if plane == 0 { (0, 0) } else { (fs.ssx, fs.ssy) };
    let (dx, dy) = if pass == 0 { (1i32, 0i32) } else { (0, 1) };
    let x = col * 4;
    let y = row * 4;
    let row = row | sub_y;
    let col = col | sub_x;
    let on_screen = !(x >= fs.fh.frame_width as usize || y >= fs.fh.frame_height as usize || (pass == 0 && x == 0) || (pass == 1 && y == 0));
    if !on_screen {
        return;
    }
    let xp = x >> sub_x;
    let yp = y >> sub_y;
    let prev_row = row - ((dy as usize) << sub_y);
    let prev_col = col - ((dx as usize) << sub_x);
    let tx_sz = fs.lf_tx_size(plane, row >> sub_y, col >> sub_x);
    let is_tx_edge = if pass == 0 { xp.is_multiple_of(TX_WIDTH[tx_sz] as usize) } else { yp.is_multiple_of(TX_HEIGHT[tx_sz] as usize) };
    if !is_tx_edge {
        return;
    }
    let mi = &fs.mi;
    let i = mi.idx(row, col);
    let mi_size = mi.mi_size[i] as usize;
    let plane_size = fs.plane_residual_size(mi_size, plane);
    let skip = mi.skip[i];
    let is_intra = mi.ref_frame[i][0] <= INTRA_FRAME as i8;
    let is_block_edge = if pass == 0 {
        xp.is_multiple_of(4 * NUM_4X4_BLOCKS_WIDE[plane_size] as usize)
    } else {
        yp.is_multiple_of(4 * NUM_4X4_BLOCKS_HIGH[plane_size] as usize)
    };
    if !(is_block_edge || !skip || is_intra) {
        return;
    }
    let prev_tx_sz = fs.lf_tx_size(plane, prev_row >> sub_y, prev_col >> sub_x);
    // filter size (7.14.3)
    let base_size = if pass == 0 { TX_WIDTH[prev_tx_sz].min(TX_WIDTH[tx_sz]) } else { TX_HEIGHT[prev_tx_sz].min(TX_HEIGHT[tx_sz]) } as usize;
    let filter_size = if plane == 0 { 16.min(base_size) } else { 8.min(base_size) };
    let (mut lvl, mut limit, mut blimit, mut thresh) = strength(fs, row, col, plane, pass);
    if lvl == 0 {
        (lvl, limit, blimit, thresh) = strength(fs, prev_row, prev_col, plane, pass);
    }
    if lvl == 0 {
        return;
    }
    let t = LfThresh::new(limit, blimit, thresh, fs.bit_depth);
    let chroma = plane > 0;
    let stride = band.stride;
    if let Some(above) = above.filter(|_| pass == 1 && yp == band.y0) {
        // The p side is in the rows above the band: filter each line in a copy that spans both.
        let (n_above, n_band) = (above.data.len() / stride.max(1), band.data.len() / stride.max(1));
        for k in 0..4 {
            let x = xp + k;
            let mut line = [0u16; 2 * LF_LINE];
            for j in 1..=n_above.min(LF_LINE) {
                line[LF_LINE - j] = above.data[(n_above - j) * stride + x];
            }
            for j in 0..n_band.min(LF_LINE) {
                line[LF_LINE + j] = band.data[j * stride + x];
            }
            sample_filter(&mut line, LF_LINE, 1, chroma, filter_size, t);
            for j in 1..=n_above.min(LF_LINE) {
                above.data[(n_above - j) * stride + x] = line[LF_LINE - j];
            }
            for j in 0..n_band.min(LF_LINE) {
                band.data[j * stride + x] = line[LF_LINE + j];
            }
        }
        return;
    }
    let (step, along) = if pass == 0 { (1, stride) } else { (stride, 1) };
    let base = (yp - band.y0) * stride + xp;
    for k in 0..4 {
        sample_filter(band.data, base + k * along, step, chroma, filter_size, t);
    }
}

/// Samples on each side of an edge that the sample filter can read (at most p6 .. q6).
const LF_LINE: usize = 8;

/// Adaptive filter strength (7.14.4): (lvl, limit, blimit, thresh).
fn strength(fs: &FrameState, row: usize, col: usize, plane: usize, pass: usize) -> (i32, i32, i32, i32) {
    let mi = &fs.mi;
    let i = mi.idx(row, col);
    let segment = mi.segment_id[i] as usize;
    let rf = mi.ref_frame[i][0];
    let mode = mi.y_mode[i] as usize;
    let mode_type = (mode >= NEARESTMV && mode != GLOBALMV && mode != GLOBAL_GLOBALMV) as usize;
    let delta_lf = if fs.fh.delta_lf_multi { mi.delta_lf[i][if plane == 0 { pass } else { plane + 1 }] } else { mi.delta_lf[i][0] } as i32;
    // 7.14.5
    let idx = if plane == 0 { pass } else { plane + 1 };
    let base = (delta_lf + fs.fh.lf.level[idx] as i32).clamp(0, MAX_LOOP_FILTER as i32);
    let mut lvl_seg = base;
    let feature = SEG_LVL_ALT_LF_Y_V + idx;
    if fs.fh.seg.enabled && fs.fh.seg.features.enabled[segment][feature] {
        lvl_seg = (fs.fh.seg.features.data[segment][feature] + lvl_seg).clamp(0, MAX_LOOP_FILTER as i32);
    }
    if fs.fh.lf.delta_enabled {
        let n_shift = lvl_seg >> 5;
        let d = &fs.fh.lf.deltas;
        if rf == INTRA_FRAME as i8 {
            lvl_seg += d.ref_deltas[INTRA_FRAME] << n_shift;
        } else if rf > INTRA_FRAME as i8 {
            lvl_seg += (d.ref_deltas[rf as usize] << n_shift) + (d.mode_deltas[mode_type] << n_shift);
        }
        lvl_seg = lvl_seg.clamp(0, MAX_LOOP_FILTER as i32);
    }
    let lvl = lvl_seg;
    let sharp = fs.fh.lf.sharpness as i32;
    let shift = if sharp > 4 {
        2
    } else if sharp > 0 {
        1
    } else {
        0
    };
    let limit = if sharp > 0 { (lvl >> shift).clamp(1, 9 - sharp) } else { (lvl >> shift).max(1) };
    let blimit = 2 * (lvl + 2) + limit;
    let thresh = lvl >> 4;
    (lvl, limit, blimit, thresh)
}

/// Per-edge thresholds of the sample filter, scaled to the bit depth.
#[derive(Clone, Copy)]
struct LfThresh {
    limit: i32,
    blimit: i32,
    thresh: i32,
    /// Flatness threshold 1 << (BitDepth - 8).
    flat: i32,
    /// 0x80 << (BitDepth - 8) and 1 << (BitDepth - 1).
    off: i32,
    half: i32,
}

impl LfThresh {
    fn new(limit: i32, blimit: i32, thresh: i32, bd: u32) -> LfThresh {
        let sh = bd - 8;
        LfThresh { limit: limit << sh, blimit: blimit << sh, thresh: thresh << sh, flat: 1 << sh, off: 0x80 << sh, half: 1 << (bd - 1) }
    }
}

/// Sample filtering process (7.14.6) for one line across the edge: `base` indexes q0 and
/// `step` is the distance between samples across the edge.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn sample_filter(d: &mut [u16], base: usize, step: usize, chroma: bool, filter_size: usize, t: LfThresh) {
    let at = |d: &[u16], k: isize| -> i32 { d[(base as isize + k * step as isize) as usize] as i32 };
    let (q0, q1, p0, p1) = (at(d, 0), at(d, 1), at(d, -1), at(d, -2));
    let lim = t.limit;
    // filter mask (7.14.6.2)
    if (p1 - p0).abs() > lim || (q1 - q0).abs() > lim || (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 > t.blimit {
        return;
    }
    let hev = (p1 - p0).abs() > t.thresh || (q1 - q0).abs() > t.thresh;
    let mut flat = false;
    if filter_size > 4 {
        let (q2, p2) = (at(d, 2), at(d, -3));
        if (p2 - p1).abs() > lim || (q2 - q1).abs() > lim {
            return;
        }
        let tf = t.flat;
        flat = (p1 - p0).abs() <= tf && (q1 - q0).abs() <= tf && (p2 - p0).abs() <= tf && (q2 - q0).abs() <= tf;
        if !chroma {
            // filterLen >= 8
            let (q3, p3) = (at(d, 3), at(d, -4));
            if (p3 - p2).abs() > lim || (q3 - q2).abs() > lim {
                return;
            }
            flat = flat && (p3 - p0).abs() <= tf && (q3 - q0).abs() <= tf;
        }
    }
    if !flat {
        // narrow filter (7.14.6.3)
        let c = |v: i32| v.clamp(-t.half, t.half - 1);
        let off = t.off;
        let (ps1, ps0, qs0, qs1) = (p1 - off, p0 - off, q0 - off, q1 - off);
        let mut filter = if hev { c(ps1 - qs1) } else { 0 };
        filter = c(filter + 3 * (qs0 - ps0));
        let filter1 = c(filter + 4) >> 3;
        let filter2 = c(filter + 3) >> 3;
        d[base] = (c(qs0 - filter1) + off) as u16;
        d[base - step] = (c(ps0 + filter2) + off) as u16;
        if !hev {
            let f = (filter1 + 1) >> 1;
            d[base + step] = (c(qs1 - f) + off) as u16;
            d[base - 2 * step] = (c(ps1 + f) + off) as u16;
        }
        return;
    }
    if chroma {
        wide::<2, 1, 3>(d, base, step);
    } else if filter_size == 8 {
        wide::<3, 0, 3>(d, base, step);
    } else {
        let tf = t.flat;
        let q0 = at(d, 0);
        let p0 = at(d, -1);
        let flat2 = (4..7).all(|k| (at(d, k) - q0).abs() <= tf && (at(d, -k - 1) - p0).abs() <= tf);
        if flat2 { wide::<6, 1, 4>(d, base, step) } else { wide::<3, 0, 3>(d, base, step) }
    }
}

/// Wide filter process (7.14.6.4) with n = N, the 2-weight taps |j| <= N2 and log2Size = LOG2.
#[inline(always)]
fn wide<const N: usize, const N2: usize, const LOG2: u32>(d: &mut [u16], base: usize, step: usize) {
    // v[k] = sample at offset k - (N + 1) from q0, k in 0 .. 2N + 2
    let mut v = [0i32; 14];
    let first = base - (N + 1) * step;
    for (k, x) in v[..2 * N + 2].iter_mut().enumerate() {
        *x = d[first + k * step] as i32;
    }
    // e[m] = v[clamp(m - N, 0, 2N + 1)]: the clamped tap positions as one array, so
    // F[i] = sum_{jj = 0}^{2N} e[i + 1 + jj] * (2 if |jj - N| <= N2 else 1), a sliding window.
    let mut e = [0i32; 25];
    for (m, x) in e[..4 * N + 1].iter_mut().enumerate() {
        *x = v[m.saturating_sub(N).min(2 * N + 1)];
    }
    let mut f = [0i32; 12];
    let mut win: i32 = e[1..2 * N + 2].iter().sum();
    for i in 0..2 * N {
        if i > 0 {
            win += e[i + 1 + 2 * N] - e[i];
        }
        let centre: i32 = e[i + 1 + N - N2..=i + 1 + N + N2].iter().sum();
        f[i] = (win + centre + (1 << (LOG2 - 1))) >> LOG2;
    }
    let out = base - N * step;
    for i in 0..2 * N {
        d[out + i * step] = f[i] as u16;
    }
}
