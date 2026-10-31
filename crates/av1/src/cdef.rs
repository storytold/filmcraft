//! CDEF (7.15), in place.
//!
//! Every block reads CurrFrame up to 2 samples around itself, so filtering in place delays
//! writes until the neighbours have read: a block's output is written once the block to its
//! right has read its samples, except its last 2 rows, which wait until the next block row has
//! been read. Only the rows on either side of a band are copies (saved before the bands run in
//! parallel).

use crate::frame::{FrameBuf, RowBand};
use crate::par::Pool;
use crate::postfilter::bands;
use crate::spec_tables::{CDEF_DIRECTIONS, CDEF_PRI_TAPS, CDEF_SEC_TAPS, CDEF_UV_DIR};
use crate::state::FrameState;
use filmcraft_simd::CdefTap;

/// Unfiltered rows a band of one plane reads outside itself: the 2 rows above and below it
/// (each empty at the top or bottom of the plane).
type BandEdges = [Vec<u16>; 2];

/// Run CDEF on `cur` (CurrFrame, which becomes CdefFrame). Bands of 64 luma rows (whole 8x8
/// blocks) are filtered in parallel.
pub(crate) fn apply(fs: &FrameState, cur: &mut FrameBuf, pool: &Pool) {
    let fh = &fs.fh;
    if fh.coded_lossless || fh.allow_intrabc || !fs.seq.enable_cdef {
        return;
    }
    let ranges = bands(pool, fh.mi_rows as usize * 4);
    let edges: Vec<[BandEdges; 3]> = ranges
        .iter()
        .map(|&(start, end)| {
            std::array::from_fn(|p| {
                let pl = &cur.planes[p];
                let sub_y = if p == 0 { 0 } else { fs.ssy };
                let (s, e) = (start >> sub_y, end >> sub_y);
                let rows = |y0: usize, y1: usize| pl.data.get(y0 * pl.stride..y1.min(pl.rows) * pl.stride).map(<[u16]>::to_vec).unwrap_or_default();
                [if s == 0 { Vec::new() } else { rows(s - 2, s) }, rows(e.min(pl.rows), e.saturating_add(2))]
            })
        })
        .collect();
    let mut planes = cur.row_bands(&ranges);
    pool.map_mut(&mut planes, |b, band| filter_band(fs, band, ranges[b], &edges[b]));
}

/// A filtered block waiting to be written: its position and samples (w per row).
struct Pending {
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    out: [u16; 64],
}

/// Rows `rows` of `out` (w samples each) into `dst` from row `y0`, column `x0`.
fn put_rows(out: &[u16], w: usize, rows: std::ops::Range<usize>, x0: usize, y0: usize, dst: &mut RowBand) {
    // Rows of a fixed length (a call to memmove otherwise).
    fn put<const W: usize>(out: &[u16], rows: std::ops::Range<usize>, x0: usize, y0: usize, dst: &mut RowBand) {
        for i in rows {
            let o = out.get(i * W..).and_then(|o| o.first_chunk::<W>());
            if let (Some(o), Some(d)) = (o, dst.row_mut(y0 + i).get_mut(x0..).and_then(|d| d.first_chunk_mut::<W>())) {
                *d = *o;
            }
        }
    }
    if w == 8 {
        put::<8>(out, rows, x0, y0, dst);
    } else {
        put::<4>(out, rows, x0, y0, dst);
    }
}

impl Pending {
    /// Writes all but the last 2 rows; those (which the next block row reads unfiltered) go to
    /// `tails`.
    fn write(&self, dst: &mut RowBand, tails: &mut Vec<Tail>) {
        let body = self.h - 2;
        put_rows(&self.out, self.w, 0..body, self.x0, self.y0, dst);
        let mut t = Tail { x0: self.x0, y0: self.y0 + body, w: self.w, out: [0; 16] };
        t.out[..2 * self.w].copy_from_slice(&self.out[body * self.w..self.h * self.w]);
        tails.push(t);
    }
}

/// The last 2 rows of a filtered block, written once the next block row has been read.
struct Tail {
    x0: usize,
    y0: usize,
    w: usize,
    out: [u16; 16],
}

impl Tail {
    fn write(&self, dst: &mut RowBand) {
        put_rows(&self.out, self.w, 0..2, self.x0, self.y0, dst);
    }
}

/// CurrFrame as a block row of one plane sees it while the band is filtered in place.
struct Src<'a> {
    band: &'a RowBand<'a>,
    /// The 2 rows above the band.
    above: &'a [u16],
    /// The rows from the band's end on.
    below: &'a [u16],
    end: usize,
}

impl Src<'_> {
    #[inline(always)]
    fn row(&self, y: usize) -> &[u16] {
        let st = self.band.stride;
        if y < self.band.y0 {
            let i = y + 2 - self.band.y0;
            self.above.get(i * st..(i + 1) * st).unwrap_or(&[])
        } else if y >= self.end {
            let i = y - self.end;
            self.below.get(i * st..(i + 1) * st).unwrap_or(&[])
        } else {
            self.band.row(y)
        }
    }
}

/// CDEF of the band holding luma rows [start, end), one block row (8 luma rows) at a time.
fn filter_band(fs: &FrameState, band: &mut [RowBand; 3], (start, end): (usize, usize), edges: &[BandEdges; 3]) {
    let (mi_rows, mi_cols) = (fs.fh.mi_rows as usize, fs.fh.mi_cols as usize);
    let np = fs.num_planes;
    let mut pending: [Option<Pending>; 3] = Default::default();
    // The last rows of this block row's filtered blocks, and of the previous one's.
    let (mut tails, mut prev_tails): ([Vec<Tail>; 3], [Vec<Tail>; 3]) = Default::default();
    // The first row past each plane's band (where `Src` switches to the saved rows below).
    let ends: [usize; 3] = std::array::from_fn(|p| band[p].y0 + band[p].data.len() / band[p].stride.max(1));
    let mut r = start / 4;
    while r < mi_rows.min(end / 4) {
        let mut c = 0;
        while c < mi_cols {
            let idx = fs.cdef_idx(r & !15, c & !15);
            if idx >= 0 {
                cdef_block(fs, band, edges, (&mut pending, &mut tails), &ends, (r, c), idx as usize);
            }
            c += 2;
        }
        for p in 0..np {
            if let Some(b) = pending[p].take() {
                b.write(&mut band[p], &mut tails[p]);
            }
            // This block row has read the previous one's last rows.
            for t in prev_tails[p].drain(..) {
                t.write(&mut band[p]);
            }
        }
        std::mem::swap(&mut tails, &mut prev_tails);
        r += 2;
    }
    for p in 0..np {
        for t in prev_tails[p].drain(..) {
            t.write(&mut band[p]);
        }
    }
}

fn cdef_block(
    fs: &FrameState,
    band: &mut [RowBand; 3],
    edges: &[BandEdges; 3],
    (pending, tails): (&mut [Option<Pending>; 3], &mut [Vec<Tail>; 3]),
    ends: &[usize; 3],
    (r, c): (usize, usize),
    idx: usize,
) {
    let cp = &fs.fh.cdef;
    let chroma = fs.num_planes > 1;
    if cp.y_pri[idx] == 0 && cp.y_sec[idx] == 0 && (!chroma || (cp.uv_pri[idx] == 0 && cp.uv_sec[idx] == 0)) {
        // No plane filters (CdefFrame = CurrFrame), and the direction would go unused.
        return;
    }
    let mi = &fs.mi;
    let sk = |rr: usize, cc: usize| -> bool { if rr < mi.rows && cc < mi.cols { mi.skip[mi.idx(rr, cc)] } else { true } };
    let skip = sk(r, c) && sk(r + 1, c) && sk(r, c + 1) && sk(r + 1, c + 1);
    if skip {
        return;
    }
    let coeff_shift = fs.bit_depth - 8;
    let mut run = |plane: usize, pri0: i32, sec: i32, damping: i32, dir: Option<usize>| -> (usize, i32) {
        if let Some(d) = dir
            && pri0 == 0
            && sec == 0
        {
            // Nothing to filter and no direction to find.
            return (d, 0);
        }
        let b = &band[plane];
        let src = Src { band: b, above: &edges[plane][0], below: &edges[plane][1], end: ends[plane] };
        let (dir, var) = match dir {
            Some(d) => (d, 0),
            None => direction(fs, &src, r, c),
        };
        let pri = if plane == 0 {
            let var_str = if (var >> 6) != 0 { (31 - ((var >> 6) as u32).leading_zeros()).min(12) as i32 } else { 0 };
            if var != 0 { (pri0 * (4 + var_str) + 8) >> 4 } else { 0 }
        } else {
            pri0
        };
        // Luma filters along its direction only with a primary strength (before the variance
        // adjustment).
        let fdir = if plane == 0 && pri0 == 0 { 0 } else { dir };
        let out = filter(fs, &src, plane, (r, c), Strength { pri, sec, damping, dir: fdir });
        if let Some(o) = out
            && let Some(old) = pending[plane].replace(o)
        {
            old.write(&mut band[plane], &mut tails[plane]);
        }
        (dir, var)
    };
    let pri = (cp.y_pri[idx] << coeff_shift) as i32;
    let sec = (cp.y_sec[idx] << coeff_shift) as i32;
    let damping = cp.damping as i32 + coeff_shift as i32;
    // The direction search only matters through a primary strength: luma filters along
    // direction 0 without one (its variance only scales that strength), and so does chroma.
    let needs_dir = pri != 0 || (chroma && cp.uv_pri[idx] != 0);
    let (y_dir, _) = run(0, pri, sec, damping, if needs_dir { None } else { Some(0) });
    if fs.num_planes == 1 {
        return;
    }
    let pri = (cp.uv_pri[idx] << coeff_shift) as i32;
    let sec = (cp.uv_sec[idx] << coeff_shift) as i32;
    let dir = if pri == 0 { 0 } else { CDEF_UV_DIR[fs.ssx][fs.ssy][y_dir] as usize };
    let damping = cp.damping as i32 + coeff_shift as i32 - 1;
    run(1, pri, sec, damping, Some(dir));
    run(2, pri, sec, damping, Some(dir));
}

fn direction(fs: &FrameState, src: &Src, r: usize, c: usize) -> (usize, i32) {
    let x0 = c << 2;
    let y0 = r << 2;
    let rows: [&[u16; 8]; 8] = std::array::from_fn(|i| src.row(y0 + i).get(x0..).and_then(|s| s.first_chunk::<8>()).unwrap_or(&[0; 8]));
    filmcraft_simd::cdef_direction(&rows, fs.bit_depth - 8)
}

/// Marks a sample outside the frame (CdefAvailable = 0) in the padded block.
const CDEF_NA: i32 = i32::MIN;
/// Padded block: 8x8 plus 2 samples on every side, row stride 12.
const PAD_STRIDE: usize = filmcraft_simd::CDEF_STRIDE;

#[inline(always)]
fn constrain(diff: i32, threshold: i32, adj: u32) -> i32 {
    let mag = (threshold - (diff.abs() >> adj)).clamp(0, diff.abs());
    if diff < 0 { -mag } else { mag }
}

/// Primary/secondary strengths, damping and direction for one plane of one 8x8 block.
#[derive(Clone, Copy)]
struct Strength {
    pri: i32,
    sec: i32,
    damping: i32,
    dir: usize,
}

fn filter(fs: &FrameState, src: &Src, plane: usize, (r, c): (usize, usize), st: Strength) -> Option<Pending> {
    let Strength { pri, sec, damping, dir } = st;
    if pri == 0 && sec == 0 {
        // Every tap contributes 0 and the clamp to [min, max] keeps x: CdefFrame = CurrFrame.
        return None;
    }
    let coeff_shift = fs.bit_depth - 8;
    let (sub_x, sub_y) = if plane > 0 { (fs.ssx, fs.ssy) } else { (0, 0) };
    let x0 = (c * 4) >> sub_x;
    let y0 = (r * 4) >> sub_y;
    let w = 8 >> sub_x;
    let h = 8 >> sub_y;
    let (mi_rows, mi_cols) = (fs.fh.mi_rows as isize, fs.fh.mi_cols as isize);
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
    let mut out = Pending { x0, y0, w, h, out: [0; 64] };
    // CdefAvailable per sample: its 4x4 block is inside the frame. The padded block lies inside
    // when its first and last rows and columns do.
    let inside = |v: isize, sub: usize, mi: isize| v >= 0 && (v << sub) >> 2 < mi;
    let (top, left) = (y0 as isize - 2, x0 as isize - 2);
    if inside(top, sub_y, mi_rows)
        && inside(top + h as isize + 3, sub_y, mi_rows)
        && inside(left, sub_x, mi_cols)
        && inside(left + w as isize + 3, sub_x, mi_cols)
    {
        // Every tap is available: whole rows at a time (constrain() with threshold 0 is 0).
        let mut buf = [0u16; PAD_STRIDE * PAD_STRIDE];
        for (i, d) in buf.as_chunks_mut::<PAD_STRIDE>().0.iter_mut().take(h + 4).enumerate() {
            let s = src.row(y0 + i - 2).get(x0 - 2..).unwrap_or(&[]);
            // Whole rows of a fixed length (a call to memcpy otherwise).
            if w == 8 {
                if let Some(s) = s.first_chunk::<12>() {
                    *d = *s;
                }
            } else if let (Some(s), Some(d)) = (s.first_chunk::<8>(), d.first_chunk_mut::<8>()) {
                *d = *s;
            }
        }
        let t = Taps { pri_off, sec_off, pri_taps, sec_taps, pri, sec, pri_adj, sec_adj };
        filmcraft_simd::cdef_filter(&buf, w, h, inside_taps(&t, &mut [CdefTap::default(); 12]), t.pri != 0 && t.sec != 0, &mut out.out);
        return Some(out);
    }
    let mut buf = [CDEF_NA; PAD_STRIDE * PAD_STRIDE];
    for i in 0..h + 4 {
        let y = top + i as isize;
        if !inside(y, sub_y, mi_rows) {
            continue;
        }
        let row = src.row(y as usize);
        for j in 0..w + 4 {
            let x = left + j as isize;
            if inside(x, sub_x, mi_cols) {
                buf[i * PAD_STRIDE + j] = row[x as usize] as i32;
            }
        }
    }
    for i in 0..h {
        // The centre sample is read even where CdefAvailable is 0 (8x8 blocks straddling an odd
        // MiRows / MiCols edge).
        let srow = &src.row(y0 + i)[x0..x0 + w];
        let drow = &mut out.out[i * w..(i + 1) * w];
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
    Some(out)
}

/// A block's filter taps: offsets in the padded block, weights, strengths and damping shifts.
struct Taps {
    pri_off: [isize; 2],
    sec_off: [[isize; 2]; 2],
    pri_taps: [i32; 2],
    sec_taps: [i32; 2],
    pri: i32,
    sec: i32,
    pri_adj: u32,
    sec_adj: u32,
}

/// The taps of a block whose taps are all available, without those of a zero strength: they
/// add 0. With one strength the tap weights sum to 12 < 16, so the result stays between the
/// centre and its farthest tap and the clamp to the taps' range changes nothing (libaom and
/// dav1d skip it too).
fn inside_taps<'a>(t: &Taps, taps: &'a mut [CdefTap; 12]) -> &'a [CdefTap] {
    let tap = |off: isize, weight: i32, strength: i32, shift: u32| CdefTap { off, weight: weight as i16, strength: strength as i16, shift };
    let mut n = 0;
    for k in 0..2 {
        for sign in [-1isize, 1] {
            if t.pri != 0 {
                taps[n] = tap(sign * t.pri_off[k], t.pri_taps[k], t.pri, t.pri_adj);
                n += 1;
            }
            if t.sec != 0 {
                taps[n] = tap(sign * t.sec_off[k][0], t.sec_taps[k], t.sec, t.sec_adj);
                taps[n + 1] = tap(sign * t.sec_off[k][1], t.sec_taps[k], t.sec, t.sec_adj);
                n += 2;
            }
        }
    }
    &taps[..n]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 7.15.3 as the spec writes it: all twelve taps, then the clamp to their range.
    fn spec_filter(buf: &[u16; PAD_STRIDE * PAD_STRIDE], t: &Taps, w: usize, h: usize) -> Vec<u16> {
        let mut out = Vec::new();
        for i in 0..h {
            for j in 0..w {
                let ci = ((i + 2) * PAD_STRIDE + j + 2) as isize;
                let x = buf[ci as usize] as i32;
                let (mut sum, mut max, mut min) = (0, x, x);
                for k in 0..2 {
                    for sign in [-1isize, 1] {
                        let p = buf[(ci + sign * t.pri_off[k]) as usize] as i32;
                        sum += t.pri_taps[k] * constrain(p - x, t.pri, t.pri_adj);
                        (max, min) = (max.max(p), min.min(p));
                        for s in 0..2 {
                            let q = buf[(ci + sign * t.sec_off[k][s]) as usize] as i32;
                            sum += t.sec_taps[k] * constrain(q - x, t.sec, t.sec_adj);
                            (max, min) = (max.max(q), min.min(q));
                        }
                    }
                }
                out.push((x + ((8 + sum - (sum < 0) as i32) >> 4)).clamp(min, max) as u16);
            }
        }
        out
    }

    /// The vector path, without the zero-strength taps and (with one strength) the clamp,
    /// gives the spec's samples: random and edge-like blocks, every strength, damping and
    /// direction, at 8, 10 and 12 bits.
    #[test]
    fn fast_path_matches_the_spec_filter() {
        let mut seed = 0x1234_5679u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let off = |d: usize, k: usize| CDEF_DIRECTIONS[d][k][0] as isize * PAD_STRIDE as isize + CDEF_DIRECTIONS[d][k][1] as isize;
        let mut clamped = 0;
        for case in 0..30_000 {
            let shift = [0, 2, 4][case % 3];
            let max = (256i32 << shift) - 1;
            let (w, h) = [(8, 8), (4, 4), (4, 8)][next() as usize % 3];
            let base = (next() % (max as u32 + 1)) as i32;
            let spread = [2, 8, 40, 300, 4096][next() as usize % 5].min(max + 1);
            let buf: [u16; PAD_STRIDE * PAD_STRIDE] = std::array::from_fn(|_| (base + (next() % spread as u32) as i32 - spread / 2).clamp(0, max) as u16);
            let pri = [0, 1, 2, 3, 5, 8, 15][next() as usize % 7] << shift;
            let sec = [0, 1, 2, 4][next() as usize % 4] << shift;
            if pri == 0 && sec == 0 {
                continue;
            }
            let damping = 3 + (next() % 4) as i32 + shift;
            let dir = next() as usize % 8;
            let pri_tap = ((pri >> shift) & 1) as usize;
            let adj = |t: i32| if t == 0 { 0 } else { (damping - (31 - (t as u32).leading_zeros()) as i32).max(0) as u32 };
            let (d_lo, d_hi) = ((dir + 6) & 7, (dir + 2) & 7);
            let t = Taps {
                pri_off: [off(dir, 0), off(dir, 1)],
                sec_off: [[off(d_lo, 0), off(d_hi, 0)], [off(d_lo, 1), off(d_hi, 1)]],
                pri_taps: [CDEF_PRI_TAPS[pri_tap][0] as i32, CDEF_PRI_TAPS[pri_tap][1] as i32],
                sec_taps: [CDEF_SEC_TAPS[pri_tap][0] as i32, CDEF_SEC_TAPS[pri_tap][1] as i32],
                pri,
                sec,
                pri_adj: adj(pri),
                sec_adj: adj(sec),
            };
            let want = spec_filter(&buf, &t, w, h);
            let mut out = [0u16; 64];
            filmcraft_simd::cdef_filter(&buf, w, h, inside_taps(&t, &mut [CdefTap::default(); 12]), t.pri != 0 && t.sec != 0, &mut out);
            let got = out[..w * h].to_vec();
            assert_eq!(got, want, "case {case}: {w}x{h} pri {pri} sec {sec} damping {damping} dir {dir}");
            clamped += (pri != 0 && sec != 0) as u32;
        }
        assert!(clamped > 5000);
    }
}
