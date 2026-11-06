//! Block inter prediction process (8.5.2.4): separable 8-tap sub-sample interpolation with edge
//! clamping, for unscaled (step 16) and scaled references.

use crate::frame::Frame;
use crate::tables::SUBPEL_FILTERS;

/// Filter taps of `filter` (0..3) at sub-sample position `frac` (0..15).
#[inline(always)]
fn taps(filter: u8, frac: usize) -> &'static [i16] {
    let o = (filter as usize * 16 + frac) * 8;
    &SUBPEL_FILTERS[o..o + 8]
}

/// Plane `plane` of a reference frame, valid for x <= last_x, y <= last_y. Rows are read through
/// [`Frame::row`] / [`Frame::span`], which wait until the reference has published them.
pub struct RefPlane<'a> {
    pub frame: &'a Frame,
    pub plane: usize,
    pub last_x: i32,
    pub last_y: i32,
}

impl RefPlane<'_> {
    #[inline]
    fn row(&self, y: usize) -> &[u16] {
        self.frame.row(self.plane, y)
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

/// Predict a `w` x `h` block whose top-left sample is at (`x`, `y`) in 1/16 sample units of the
/// reference, stepping `step_x` / `step_y` (16 = unscaled). Output samples go to `out` (stride
/// `w`). `tmp` must hold at least `(h * step_y / 16 + 8) * w` entries, `win` at least
/// `(w + 7) * (h + 7)` (edge-clamped source window).
pub fn predict(r: &RefPlane, blk: &McBlock, out: &mut [u16], tmp: &mut [u16], win: &mut [u16]) {
    if blk.step_x == 16 && blk.step_y == 16 {
        predict_unscaled(r, blk, out, tmp, win);
    } else {
        predict_scaled(r, blk, out, tmp);
    }
}

/// Copy the (w + 7) x (h + 7) source window starting at (x0 - 3, y0 - 3) with edge clamping.
fn fetch_window(r: &RefPlane, x0: i32, y0: i32, w: usize, h: usize, win: &mut [u16]) -> usize {
    let ws = w + 7;
    for row in 0..h + 7 {
        let yy = (y0 - 3 + row as i32).clamp(0, r.last_y) as usize;
        let src = r.row(yy);
        let dst = &mut win[row * ws..row * ws + ws];
        let xs = x0 - 3;
        if xs >= 0 && xs + ws as i32 - 1 <= r.last_x {
            dst.copy_from_slice(&src[xs as usize..xs as usize + ws]);
        } else {
            for (c, d) in dst.iter_mut().enumerate() {
                *d = src[(xs + c as i32).clamp(0, r.last_x) as usize];
            }
        }
    }
    ws
}

fn predict_unscaled(r: &RefPlane, blk: &McBlock, out: &mut [u16], tmp: &mut [u16], win: &mut [u16]) {
    let McBlock { x, y, w, h, filter, bit_depth, .. } = *blk;
    let (x0, y0) = (x >> 4, y >> 4);
    let (fx, fy) = ((x & 15) as usize, (y & 15) as usize);
    let max = (1i32 << bit_depth) - 1;
    let inside = x0 - 3 >= 0 && y0 - 3 >= 0 && x0 + w as i32 + 4 <= r.last_x && y0 + h as i32 + 4 <= r.last_y;
    // Source view with origin at (x0 - 3, y0 - 3): the reference band itself when the window
    // lies inside the picture and inside one band.
    let span = if inside { r.frame.span(r.plane, (y0 - 3) as usize, (y0 + h as i32 + 5) as usize) } else { None };
    if inside && span.is_none() && predict_two_bands(r, x0, y0, w, h, fx, fy, filter, max, out, tmp) {
        return;
    }
    let (src, ss, so): (&[u16], usize, usize) = match span {
        Some((data, o)) => (data, r.frame.strides[r.plane], o + (x0 - 3) as usize),
        None => {
            let ws = fetch_window(r, x0, y0, w, h, win);
            (&win[..], ws, 0)
        }
    };
    match (fx, fy) {
        (0, 0) => {
            for i in 0..h {
                let s = so + (i + 3) * ss + 3;
                out[i * w..i * w + w].copy_from_slice(&src[s..s + w]);
            }
        }
        (_, 0) => {
            let f = taps(filter, fx);
            for i in 0..h {
                filter_h(&src[so + (i + 3) * ss..], f, max, &mut out[i * w..i * w + w]);
            }
        }
        (0, _) => {
            let f = taps(filter, fy);
            for i in 0..h {
                filter_v(&src[so + i * ss + 3..], ss, f, max, &mut out[i * w..i * w + w]);
            }
        }
        _ => {
            let fh = taps(filter, fx);
            let fv = taps(filter, fy);
            for i in 0..h + 7 {
                filter_h(&src[so + i * ss..], fh, max, &mut tmp[i * w..i * w + w]);
            }
            for i in 0..h {
                filter_v(&tmp[i * w..], w, fv, max, &mut out[i * w..i * w + w]);
            }
        }
    }
}

/// [`predict_unscaled`] for a window inside the picture whose rows lie in two consecutive bands:
/// rows are read from the bands directly instead of being copied into a window first. False
/// (nothing written) if the window spans more than two bands.
#[allow(clippy::too_many_arguments)]
fn predict_two_bands(r: &RefPlane, x0: i32, y0: i32, w: usize, h: usize, fx: usize, fy: usize, filter: u8, max: i32, out: &mut [u16], tmp: &mut [u16]) -> bool {
    let f = r.frame;
    let p = r.plane;
    let bh = f.band_h[p];
    let ss = f.strides[p];
    let (ya, yb) = ((y0 - 3) as usize, (y0 + h as i32 + 4) as usize + 1);
    let split = (ya / bh + 1) * bh;
    if split >= yb {
        return false;
    }
    let (Some((d0, o0)), Some((d1, o1))) = (f.span(p, ya, split), f.span(p, split, yb)) else { return false };
    let xo = (x0 - 3) as usize;
    let row = |i: usize| -> &[u16] {
        let y = ya + i;
        if y < split { &d0[o0 + i * ss + xo..] } else { &d1[o1 + (y - split) * ss + xo..] }
    };
    match (fx, fy) {
        (0, 0) => {
            for i in 0..h {
                out[i * w..i * w + w].copy_from_slice(&row(i + 3)[3..3 + w]);
            }
        }
        (_, 0) => {
            let fh = taps(filter, fx);
            for i in 0..h {
                filter_h(row(i + 3), fh, max, &mut out[i * w..i * w + w]);
            }
        }
        (0, _) => {
            let fv = taps(filter, fy);
            for i in 0..h {
                filter_v_rows(std::array::from_fn(|t| &row(i + t)[3..]), fv, max, &mut out[i * w..i * w + w]);
            }
        }
        _ => {
            let fh = taps(filter, fx);
            let fv = taps(filter, fy);
            for i in 0..h + 7 {
                filter_h(row(i), fh, max, &mut tmp[i * w..i * w + w]);
            }
            for i in 0..h {
                filter_v(&tmp[i * w..], w, fv, max, &mut out[i * w..i * w + w]);
            }
        }
    }
    true
}

/// Horizontal 8-tap filter of one row: `o[j]` from `s[j..j + 8]`. The taps are the outer loop
/// so that the row vectorizes (same sums, same order of the exact integer additions).
#[inline(always)]
fn filter_h(s: &[u16], f: &[i16], max: i32, o: &mut [u16]) {
    match o.len() {
        4 => filter_h_n::<4>(s, f, max, o),
        8 => filter_h_n::<8>(s, f, max, o),
        16 => filter_h_n::<16>(s, f, max, o),
        32 => filter_h_n::<32>(s, f, max, o),
        64 => filter_h_n::<64>(s, f, max, o),
        _ => {
            for (j, d) in o.iter_mut().enumerate() {
                let acc: i32 = (0..8).map(|t| f[t] as i32 * s[j + t] as i32).sum();
                *d = ((acc + 64) >> 7).clamp(0, max) as u16;
            }
        }
    }
}

#[inline(always)]
fn filter_h_n<const W: usize>(s: &[u16], f: &[i16], max: i32, o: &mut [u16]) {
    let Some(o) = o.first_chunk_mut::<W>() else {
        return;
    };
    if max == 255 {
        let mut acc = [64u16; W];
        for t in 0..8 {
            let c = f[t] as u16;
            let Some(p) = s.get(t..).and_then(|s| s.first_chunk::<W>()) else {
                return;
            };
            for j in 0..W {
                acc[j] = acc[j].wrapping_add(c.wrapping_mul(p[j]));
            }
        }
        narrow8(&acc, o);
        return;
    }
    let mut acc = [64i32; W];
    for t in 0..8 {
        let c = f[t] as i32;
        let Some(p) = s.get(t..).and_then(|s| s.first_chunk::<W>()) else {
            return;
        };
        for j in 0..W {
            acc[j] += c * p[j] as i32;
        }
    }
    for j in 0..W {
        o[j] = (acc[j] >> 7).clamp(0, max) as u16;
    }
}

/// Round2(sum, 7) clipped to 8 bits, from 8-bit filter sums kept modulo 2^16 (8 lanes per
/// vector instead of 4). With 8-bit samples every sum of the VP9 filters (whose positive taps
/// add up to at most 182 and negative taps to at most -54) plus the rounding term 64 lies in
/// [-13706, 46474]: wrapped, the non-negative ones stay below 0xC000 and the negative ones
/// (which clip to 0) land above it.
#[inline(always)]
fn narrow8<const W: usize>(acc: &[u16; W], o: &mut [u16; W]) {
    for j in 0..W {
        o[j] = if acc[j] < 0xC000 { (acc[j] >> 7).min(255) } else { 0 };
    }
}

/// Vertical 8-tap filter of one row: `o[j]` from `s[j + t * ss]`, t = 0..8.
#[inline(always)]
fn filter_v(s: &[u16], ss: usize, f: &[i16], max: i32, o: &mut [u16]) {
    filter_v_rows(std::array::from_fn(|t| s.get(t * ss..).unwrap_or_default()), f, max, o)
}

/// Vertical 8-tap filter of one row: `o[j]` from `rows[t][j]`, t = 0..8.
#[inline(always)]
fn filter_v_rows(rows: [&[u16]; 8], f: &[i16], max: i32, o: &mut [u16]) {
    match o.len() {
        4 => filter_v_n::<4>(&rows, f, max, o),
        8 => filter_v_n::<8>(&rows, f, max, o),
        16 => filter_v_n::<16>(&rows, f, max, o),
        32 => filter_v_n::<32>(&rows, f, max, o),
        64 => filter_v_n::<64>(&rows, f, max, o),
        _ => {
            for (j, d) in o.iter_mut().enumerate() {
                let acc: i32 = (0..8).map(|t| f[t] as i32 * rows[t][j] as i32).sum();
                *d = ((acc + 64) >> 7).clamp(0, max) as u16;
            }
        }
    }
}

#[inline(always)]
fn filter_v_n<const W: usize>(rows: &[&[u16]; 8], f: &[i16], max: i32, o: &mut [u16]) {
    let Some(o) = o.first_chunk_mut::<W>() else {
        return;
    };
    if max == 255 {
        let mut acc = [64u16; W];
        for t in 0..8 {
            let c = f[t] as u16;
            let Some(p) = rows[t].first_chunk::<W>() else {
                return;
            };
            for j in 0..W {
                acc[j] = acc[j].wrapping_add(c.wrapping_mul(p[j]));
            }
        }
        narrow8(&acc, o);
        return;
    }
    let mut acc = [64i32; W];
    for t in 0..8 {
        let c = f[t] as i32;
        let Some(p) = rows[t].first_chunk::<W>() else {
            return;
        };
        for j in 0..W {
            acc[j] += c * p[j] as i32;
        }
    }
    for j in 0..W {
        o[j] = (acc[j] >> 7).clamp(0, max) as u16;
    }
}

/// General process of 8.5.2.4 (any step).
fn predict_scaled(r: &RefPlane, blk: &McBlock, out: &mut [u16], tmp: &mut [u16]) {
    let McBlock { x, y, step_x, step_y, w, h, filter, bit_depth } = *blk;
    let max = (1i32 << bit_depth) - 1;
    let ih = ((((h as i32 - 1) * step_y + 15) >> 4) + 8) as usize;
    for row in 0..ih {
        let yy = ((y >> 4) + row as i32 - 3).clamp(0, r.last_y) as usize;
        let src = r.row(yy);
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
                    predict_unscaled(&r, &McBlock { x, y, step_x: 16, step_y: 16, w: bw, h: bh, filter, bit_depth: 8 }, &mut a, &mut tmp, &mut [0u16; 71 * 71]);
                    predict_scaled(&r, &McBlock { x, y, step_x: 16, step_y: 16, w: bw, h: bh, filter, bit_depth: 8 }, &mut b, &mut tmp);
                    assert_eq!(a, b, "filter {filter} pos ({x},{y}) {bw}x{bh}");
                }
            }
        }
    }
}
