//! Block inter prediction process (8.5.2.4): separable 8-tap sub-sample interpolation with edge
//! clamping, for unscaled (step 16; the filters are `filmcraft_simd::vp9_predict`) and scaled
//! references.

use crate::frame::Frame;
use crate::tables::SUBPEL_FILTERS;
pub use filmcraft_simd::Vp9Out;

/// Filter taps of `filter` (0..3) at sub-sample position `frac` (0..15).
#[inline(always)]
fn taps(filter: u8, frac: usize) -> &'static [i16; 8] {
    const NONE: [i16; 8] = [0, 0, 0, 128, 0, 0, 0, 0];
    let o = (filter as usize * 16 + frac) * 8;
    SUBPEL_FILTERS.get(o..).and_then(|f| f.first_chunk::<8>()).unwrap_or(&NONE)
}

/// Plane `plane` of a reference frame, valid for x <= last_x, y <= last_y. Rows are read through
/// [`Frame::row`] / [`Frame::span`], which wait until the reference has published them.
pub struct RefPlane<'a> {
    pub frame: &'a Frame,
    pub plane: usize,
    pub last_x: i32,
    pub last_y: i32,
}

impl<'a> RefPlane<'a> {
    /// A row reader resolving once whether the reference is in whole planes or in bands.
    #[inline]
    fn rows(&self) -> impl Fn(usize) -> &'a [u16] + use<'a> {
        let (f, p) = (self.frame, self.plane);
        let (whole, s) = (f.whole(p), f.strides[p]);
        move |y| match whole {
            Some(w) => w.get(y * s..y * s + s).unwrap_or_default(),
            None => f.band_row(p, y),
        }
    }
}

/// Geometry and filtering parameters of one motion-compensated block.
#[derive(Clone, Copy, Debug)]
pub struct McBlock {
    /// Top-left sample position in 1/16 sample units of the reference.
    pub x: i32,
    pub y: i32,
    /// Step per output sample in 1/16 units (16 = unscaled).
    pub step_x: i32,
    pub step_y: i32,
    pub w: usize,
    pub h: usize,
    pub filter: u8,
    pub bit_depth: u8,
}

/// Predict the block `blk` (`w` 4, 8, 16, 32 or 64) of an unscaled reference, written into `dst`
/// (rows `ds` apart) as `out` says. `win` must hold at least `(w + 7) * (h + 7) + 1` entries
/// (edge-clamped source window, plus the one sample the vector loads read past it).
pub fn predict_unscaled(r: &RefPlane, blk: &McBlock, dst: &mut [u16], ds: usize, out: Vp9Out, win: &mut [u16]) {
    let McBlock { x, y, w, h, filter, bit_depth, .. } = *blk;
    let (x0, y0) = (x >> 4, y >> 4);
    let (fx, fy) = ((x & 15) as usize, (y & 15) as usize);
    let inside = x0 - 3 >= 0 && y0 - 3 >= 0 && x0 + w as i32 + 4 <= r.last_x && y0 + h as i32 + 4 <= r.last_y;
    // Source view with origin at (x0 - 3, y0 - 3): the reference itself when the window lies
    // inside the picture (and, while the reference is unfinished, inside one band); a window
    // copy with the edges clamped only past the picture's edges.
    let span = if inside { r.frame.span(r.plane, (y0 - 3) as usize, (y0 + h as i32 + 5) as usize) } else { None };
    let (src, ss): (&[u16], usize) = match span {
        Some((data, o)) => (data.get(o + (x0 - 3) as usize..).unwrap_or_default(), r.frame.strides[r.plane]),
        None => {
            let ws = fetch_window(r, x0, y0, w, h, win);
            (&win[..], ws)
        }
    };
    let (fh, fv) = ((fx != 0).then(|| taps(filter, fx)), (fy != 0).then(|| taps(filter, fy)));
    filmcraft_simd::vp9_predict((src, ss), (fh, fv), (w, h), ((1u32 << bit_depth) - 1) as u16, (dst, ds), out);
}

/// `dst[..n] = src[..n]` with a fixed-length copy for the block widths (and window widths,
/// + 7) that occur, so that each row is a few inline loads and stores rather than a memcpy call.
#[inline(always)]
pub(crate) fn copy_samples(dst: &mut [u16], src: &[u16], n: usize) {
    #[inline(always)]
    fn fixed<const N: usize>(dst: &mut [u16], src: &[u16]) {
        if let (Some(d), Some(s)) = (dst.first_chunk_mut::<N>(), src.first_chunk::<N>()) {
            *d = *s;
        }
    }
    match n {
        4 => fixed::<4>(dst, src),
        8 => fixed::<8>(dst, src),
        16 => fixed::<16>(dst, src),
        32 => fixed::<32>(dst, src),
        64 => fixed::<64>(dst, src),
        11 => fixed::<11>(dst, src),
        15 => fixed::<15>(dst, src),
        23 => fixed::<23>(dst, src),
        39 => fixed::<39>(dst, src),
        71 => fixed::<71>(dst, src),
        _ => copy_samples_any(dst, src, n),
    }
}

/// [`copy_samples`] of any length, out of line: inlined, the compiler merges the fixed-length
/// copies with this variable-length one into one memcpy call of a run-time length.
#[cold]
#[inline(never)]
fn copy_samples_any(dst: &mut [u16], src: &[u16], n: usize) {
    if let (Some(d), Some(s)) = (dst.get_mut(..n), src.get(..n)) {
        d.copy_from_slice(s);
    }
}

/// Copy the (w + 7) x (h + 7) source window starting at (x0 - 3, y0 - 3) with edge clamping.
fn fetch_window(r: &RefPlane, x0: i32, y0: i32, w: usize, h: usize, win: &mut [u16]) -> usize {
    let ws = w + 7;
    let rows = r.rows();
    for row in 0..h + 7 {
        let yy = (y0 - 3 + row as i32).clamp(0, r.last_y) as usize;
        let src = rows(yy);
        let dst = &mut win[row * ws..row * ws + ws];
        let xs = x0 - 3;
        let interior = if xs >= 0 && xs + ws as i32 - 1 <= r.last_x { src.get(xs as usize..xs as usize + ws) } else { None };
        if let Some(s) = interior {
            copy_samples(dst, s, ws);
        } else {
            // Columns left of the picture, inside it, and right of it.
            let lo = (-xs).clamp(0, ws as i32) as usize;
            let hi = (r.last_x + 1 - xs).clamp(lo as i32, ws as i32) as usize;
            let edge = |x: i32| src.get(x as usize).copied().unwrap_or_default();
            dst[..lo].fill(edge(0));
            if let Some(inside) = src.get((xs + lo as i32) as usize..(xs + hi as i32) as usize) {
                dst[lo..hi].copy_from_slice(inside);
            }
            dst[hi..].fill(edge(r.last_x));
        }
    }
    ws
}

/// General process of 8.5.2.4 (any step): the block `blk` into `out` (stride `w`). `tmp` must
/// hold at least `(h * step_y / 16 + 8) * w` entries.
pub fn predict_scaled(r: &RefPlane, blk: &McBlock, out: &mut [u16], tmp: &mut [u16]) {
    let McBlock { x, y, step_x, step_y, w, h, filter, bit_depth } = *blk;
    let max = (1i32 << bit_depth) - 1;
    let ih = ((((h as i32 - 1) * step_y + 15) >> 4) + 8) as usize;
    let rows = r.rows();
    for row in 0..ih {
        let yy = ((y >> 4) + row as i32 - 3).clamp(0, r.last_y) as usize;
        let src = rows(yy);
        for c in 0..w {
            let p = x + step_x * c as i32;
            let f = taps(filter, (p & 15) as usize);
            let mut acc = 0i32;
            for t in 0..8 {
                acc += f[t] as i32 * src[((p >> 4) + t as i32 - 3).clamp(0, r.last_x) as usize] as i32;
            }
            tmp[row * w + c] = ((acc + 64) >> 7).clamp(0, max) as u16;
        }
    }
    for rr in 0..h {
        let p = (y & 15) + step_y * rr as i32;
        let f = taps(filter, (p & 15) as usize);
        let base = (p >> 4) as usize;
        for c in 0..w {
            let mut acc = 0i32;
            for t in 0..8 {
                acc += f[t] as i32 * tmp[(base + t) * w + c] as i32;
            }
            out[rr * w + c] = ((acc + 64) >> 7).clamp(0, max) as u16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_path_with_unit_step_matches_unscaled() {
        let (w, h) = (40usize, 30usize);
        let data: Vec<u16> = (0..w * h).map(|i| ((i * 37 + (i / w) * 11) % 256) as u16).collect();
        let info = crate::frame::FrameInfo {
            width: w as u32,
            height: h as u32,
            ss_x: true,
            ss_y: true,
            bit_depth: 8,
            color_space: 0,
            color_range: false,
            render_width: w as u32,
            render_height: h as u32,
            key: true,
            intra_only: false,
        };
        // Planes are allocated in whole superblocks: copy the test picture into a 64x64 plane.
        let mut full = vec![0u16; 64 * 64];
        for y in 0..h {
            full[y * 64..y * 64 + w].copy_from_slice(&data[y * w..y * w + w]);
        }
        let chroma = vec![0u16; 32 * 32];
        let frame = Frame::from_planes(info, [&full, &chroma, &chroma]);
        let r = RefPlane { frame: &frame, plane: 0, last_x: w as i32 - 1, last_y: h as i32 - 1 };
        let mut tmp = vec![0u16; 80 * 80];
        for filter in 0..4u8 {
            for &(x, y) in &[(0, 0), (5, 3), (16 * 7 + 5, 16 * 9 + 11), (-40, -3), (16 * 36 + 1, 16 * 25 + 15), (16 * 10, 16 * 10 + 8)] {
                for &(bw, bh) in &[(4usize, 4usize), (8, 8), (16, 8)] {
                    let mut a = vec![0u16; bw * bh];
                    let mut b = vec![0u16; bw * bh];
                    let blk = McBlock { x, y, step_x: 16, step_y: 16, w: bw, h: bh, filter, bit_depth: 8 };
                    predict_unscaled(&r, &blk, &mut a, bw, Vp9Out::Put, &mut [0u16; 71 * 71 + 1]);
                    predict_scaled(&r, &blk, &mut b, &mut tmp);
                    assert_eq!(a, b, "filter {filter} pos ({x},{y}) {bw}x{bh}");
                }
            }
        }
    }

    /// A reference finished in whole planes predicts as the same picture published in bands:
    /// blocks inside, across band boundaries (luma 64 and chroma 32 rows) and past every edge,
    /// all sizes and filter phases.
    #[test]
    fn whole_planes_predict_like_bands() {
        let (w, h) = (150usize, 140usize);
        let info = crate::frame::FrameInfo {
            width: w as u32,
            height: h as u32,
            ss_x: true,
            ss_y: true,
            bit_depth: 10,
            color_space: 0,
            color_range: false,
            render_width: w as u32,
            render_height: h as u32,
            key: true,
            intra_only: false,
        };
        // Planes of whole superblocks: 192 x 192 luma, 96 x 96 chroma.
        let plane = |n: usize, k: usize| (0..n * n).map(|i| ((i * 37 + (i / n) * 11 + k * 101) % 1024) as u16).collect::<Vec<u16>>();
        let (y, u, v) = (plane(192, 0), plane(96, 1), plane(96, 2));
        let bands = Frame::from_planes(info, [&y, &u, &v]);
        let whole = Frame::from_whole_planes(info, [&y, &u, &v]);
        let mut seed = 0x0bad_f00du32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for case in 0..4000 {
            let p = case % 3;
            let (pw, ph) = if p == 0 { (w as i32, h as i32) } else { (w.div_ceil(2) as i32, h.div_ceil(2) as i32) };
            let bw = [4usize, 8, 16, 32, 64][next() as usize % 5];
            let bh = [4usize, 8, 16, 32, 64][next() as usize % 5];
            let x = (next() % ((pw as u32 + 40) * 16)) as i32 - 20 * 16;
            let y = (next() % ((ph as u32 + 40) * 16)) as i32 - 20 * 16;
            let filter = (next() % 4) as u8;
            let mut out = [vec![0u16; bw * bh], vec![0u16; bw * bh]];
            for (f, o) in [&bands, &whole].into_iter().zip(&mut out) {
                let r = RefPlane { frame: f, plane: p, last_x: pw - 1, last_y: ph - 1 };
                predict_unscaled(
                    &r,
                    &McBlock { x, y, step_x: 16, step_y: 16, w: bw, h: bh, filter, bit_depth: 10 },
                    o,
                    bw,
                    Vp9Out::Put,
                    &mut [0u16; 71 * 71 + 1],
                );
            }
            assert_eq!(out[0], out[1], "plane {p} ({x}, {y}) {bw}x{bh} filter {filter}");
        }
        for p in 0..3 {
            for yy in 0..[192, 96, 96][p] {
                assert_eq!(bands.row(p, yy), whole.row(p, yy), "plane {p} row {yy}");
            }
        }
    }
}
