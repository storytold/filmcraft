//! Intra prediction (12) and inter prediction (18).
//!
//! Planes are the decoder's reconstruction buffers, whose size is a whole number of macroblocks.
//! Intra prediction reads the not yet loop-filtered current frame; pixels above the frame are 127
//! and pixels left of it 129 (the corner above-left of a row's first macroblock is 127 on the top
//! row and 129 below it). Inter prediction reads a reference plane extended infinitely by
//! repeating its edge pixels.

use crate::tables::{B_HD_PRED, B_HE_PRED, B_HU_PRED, B_LD_PRED, B_RD_PRED, B_TM_PRED, B_VE_PRED, B_VL_PRED, B_VR_PRED, H_PRED, TM_PRED, V_PRED};

/// A pixel of the current frame for intra prediction, with the out-of-frame values of section 12.
#[inline]
fn edge(plane: &[u8], stride: usize, x: isize, y: isize) -> u8 {
    if y < 0 {
        127
    } else if x < 0 {
        129
    } else {
        plane.get(y as usize * stride + x as usize).copied().unwrap_or(127)
    }
}

/// Predict a whole `size`x`size` block (16x16 luma or 8x8 chroma) at `(x0, y0)` with a
/// macroblock-level mode (12.2, 12.3), writing the prediction into the plane.
pub fn predict_block(plane: &mut [u8], stride: usize, x0: usize, y0: usize, size: usize, mode: u8) {
    let (xi, yi) = (x0 as isize, y0 as isize);
    let mut above = [0u8; 16];
    let mut left = [0u8; 16];
    for i in 0..size {
        above[i] = edge(plane, stride, xi + i as isize, yi - 1);
        left[i] = edge(plane, stride, xi - 1, yi + i as isize);
    }
    let p = i32::from(edge(plane, stride, xi - 1, yi - 1));
    let shift = size.trailing_zeros();
    let fill = |plane: &mut [u8], f: &dyn Fn(usize, usize) -> u8| {
        for r in 0..size {
            let start = (y0 + r) * stride + x0;
            if let Some(row) = plane.get_mut(start..start + size) {
                for (c, px) in row.iter_mut().enumerate() {
                    *px = f(r, c);
                }
            }
        }
    };
    match mode {
        V_PRED => fill(plane, &|_, c| above[c]),
        H_PRED => fill(plane, &|r, _| left[r]),
        TM_PRED => fill(plane, &|r, c| (i32::from(left[r]) + i32::from(above[c]) - p).clamp(0, 255) as u8),
        _ => {
            // DC_PRED: only the genuinely available edges count
            let sum = |v: &[u8]| v[..size].iter().map(|&x| u32::from(x)).sum::<u32>();
            let dc = match (y0 > 0, x0 > 0) {
                (true, true) => (sum(&above) + sum(&left) + (1 << shift)) >> (shift + 1),
                (true, false) => (sum(&above) + (1 << (shift - 1))) >> shift,
                (false, true) => (sum(&left) + (1 << (shift - 1))) >> shift,
                (false, false) => 128,
            } as u8;
            fill(plane, &|_, _| dc);
        }
    }
}

/// Predict the 4x4 luma subblock `b` (raster index in its macroblock) of the macroblock at
/// `(mx, my)` (in pixels) with subblock mode `mode` (12.3). `last_column`: the macroblock is the
/// rightmost of its row.
pub fn predict_subblock(plane: &mut [u8], stride: usize, mx: usize, my: usize, b: usize, mode: u8, last_column: bool) {
    let (bx, by) = (b & 3, b >> 2);
    let x = (mx + 4 * bx) as isize;
    let y = (my + 4 * by) as isize;
    // A[-1] = P, A[0..8]: the 4 pixels above and the 4 above-right
    let mut a = [0i32; 9];
    a[0] = i32::from(edge(plane, stride, x - 1, y - 1));
    for i in 0..4 {
        a[1 + i] = i32::from(edge(plane, stride, x + i as isize, y - 1));
    }
    for i in 0..4 {
        a[5 + i] = i32::from(if bx < 3 {
            edge(plane, stride, x + 4 + i as isize, y - 1)
        } else if my == 0 {
            127
        } else if last_column {
            // the macroblock's above-right pixels lie outside the frame: repeat (-1, 15)
            edge(plane, stride, mx as isize + 15, my as isize - 1)
        } else {
            // right-column subblocks all use the pixels above-right of the macroblock
            edge(plane, stride, mx as isize + 16 + i as isize, my as isize - 1)
        });
    }
    let mut l = [0i32; 4];
    for (i, v) in l.iter_mut().enumerate() {
        *v = i32::from(edge(plane, stride, x - 1, y + i as isize));
    }
    let av = |i: isize| a[(i + 1) as usize]; // A[i], i in -1..8
    // E[0..9] = L[3], L[2], L[1], L[0], P, A[0..4]
    let e = [l[3], l[2], l[1], l[0], a[0], a[1], a[2], a[3], a[4]];
    let avg3 = |x: i32, y: i32, z: i32| (x + y + y + z + 2) >> 2;
    let avg2 = |x: i32, y: i32| (x + y + 1) >> 1;
    let a3 = |i: isize| avg3(av(i - 1), av(i), av(i + 1)); // avg3p(A + i)
    let a2 = |i: isize| avg2(av(i), av(i + 1)); // avg2p(A + i)
    let e3 = |i: usize| avg3(e[i - 1], e[i], e[i + 1]);
    let e2 = |i: usize| avg2(e[i], e[i + 1]);
    // L[-1] = P
    let lv = |i: isize| if i < 0 { a[0] } else { l[i as usize] };
    let l3 = |i: isize| avg3(lv(i - 1), lv(i), lv(i + 1));
    let l2 = |i: isize| avg2(lv(i), lv(i + 1));
    let mut out = [[0i32; 4]; 4];
    match mode {
        B_TM_PRED => {
            for r in 0..4 {
                for c in 0..4 {
                    out[r][c] = (l[r] + av(c as isize) - a[0]).clamp(0, 255);
                }
            }
        }
        B_VE_PRED => {
            for c in 0..4 {
                let v = a3(c as isize);
                for row in &mut out {
                    row[c] = v;
                }
            }
        }
        B_HE_PRED => {
            for r in 0..4 {
                let v = if r == 3 { avg3(l[2], l[3], l[3]) } else { l3(r as isize) };
                out[r] = [v; 4];
            }
        }
        B_LD_PRED => {
            for r in 0..4 {
                for c in 0..4 {
                    let d = r + c;
                    out[r][c] = if d == 6 { avg3(av(6), av(7), av(7)) } else { a3(d as isize + 1) };
                }
            }
        }
        B_RD_PRED => {
            for r in 0..4 {
                for c in 0..4 {
                    // (3,0) uses E[1], each step towards the upper right one further
                    out[r][c] = e3((4 + c as isize - r as isize) as usize);
                }
            }
        }
        B_VR_PRED => {
            out[3][0] = e3(2);
            out[2][0] = e3(3);
            out[3][1] = e3(4);
            out[1][0] = e3(4);
            out[2][1] = e2(4);
            out[0][0] = e2(4);
            out[3][2] = e3(5);
            out[1][1] = e3(5);
            out[2][2] = e2(5);
            out[0][1] = e2(5);
            out[3][3] = e3(6);
            out[1][2] = e3(6);
            out[2][3] = e2(6);
            out[0][2] = e2(6);
            out[1][3] = e3(7);
            out[0][3] = e2(7);
        }
        B_VL_PRED => {
            out[0][0] = a2(0);
            out[1][0] = a3(1);
            out[2][0] = a2(1);
            out[0][1] = a2(1);
            out[1][1] = a3(2);
            out[3][0] = a3(2);
            out[2][1] = a2(2);
            out[0][2] = a2(2);
            out[3][1] = a3(3);
            out[1][2] = a3(3);
            out[2][2] = a2(3);
            out[0][3] = a2(3);
            out[3][2] = a3(4);
            out[1][3] = a3(4);
            out[2][3] = a3(5);
            out[3][3] = a3(6);
        }
        B_HD_PRED => {
            out[3][0] = e2(0);
            out[3][1] = e3(1);
            out[2][0] = e2(1);
            out[3][2] = e2(1);
            out[2][1] = e3(2);
            out[3][3] = e3(2);
            out[2][2] = e2(2);
            out[1][0] = e2(2);
            out[2][3] = e3(3);
            out[1][1] = e3(3);
            out[1][2] = e2(3);
            out[0][0] = e2(3);
            out[1][3] = e3(4);
            out[0][1] = e3(4);
            out[0][2] = e3(5);
            out[0][3] = e3(6);
        }
        B_HU_PRED => {
            out[0][0] = l2(0);
            out[0][1] = l3(1);
            out[0][2] = l2(1);
            out[1][0] = l2(1);
            out[0][3] = l3(2);
            out[1][1] = l3(2);
            out[1][2] = l2(2);
            out[2][0] = l2(2);
            out[1][3] = avg3(l[2], l[3], l[3]);
            out[2][1] = avg3(l[2], l[3], l[3]);
            out[2][2] = l[3];
            out[2][3] = l[3];
            out[3] = [l[3]; 4];
        }
        _ => {
            // B_DC_PRED
            let v = (4 + (1..5).map(|i| a[i]).sum::<i32>() + l.iter().sum::<i32>()) >> 3;
            out = [[v; 4]; 4];
        }
    }
    for (r, row) in out.iter().enumerate() {
        let start = (y as usize + r) * stride + x as usize;
        if let Some(dst) = plane.get_mut(start..start + 4) {
            for (d, &v) in dst.iter_mut().zip(row) {
                *d = v as u8;
            }
        }
    }
}

/// A reference plane for inter prediction: reads outside it repeat the nearest edge pixel.
pub struct RefPlane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub width: usize,
    pub height: usize,
}

impl RefPlane<'_> {
    #[inline]
    fn at(&self, x: isize, y: isize) -> u8 {
        let x = x.clamp(0, self.width as isize - 1) as usize;
        let y = y.clamp(0, self.height as isize - 1) as usize;
        self.data.get(y * self.stride + x).copied().unwrap_or(0)
    }
}

/// Predict the `w`x`h` block at `(bx, by)` of a plane from `reference` displaced by the motion
/// vector `(mvx, mvy)` in eighth pixels, using `filters` (8 x 6 taps: the six-tap or bilinear
/// set, 18.3), writing into `dst` (same layout as the block's plane).
pub fn predict_inter(
    reference: &RefPlane,
    dst: &mut [u8],
    dst_stride: usize,
    bx: usize,
    by: usize,
    w: usize,
    h: usize,
    mvx: i32,
    mvy: i32,
    filters: &[i32; 48],
) {
    let x0 = bx as isize + (mvx >> 3) as isize;
    let y0 = by as isize + (mvy >> 3) as isize;
    let (fx, fy) = ((mvx & 7) as usize, (mvy & 7) as usize);
    // source window: 2 before, 3 after in each direction
    const MAX: usize = 16 + 5;
    let mut src = [[0u8; MAX]; MAX];
    let full_inside = x0 >= 2 && y0 >= 2 && x0 + w as isize + 3 <= reference.width as isize && y0 + h as isize + 3 <= reference.height as isize;
    for r in 0..h + 5 {
        let yy = y0 + r as isize - 2;
        if full_inside {
            let start = yy as usize * reference.stride + x0 as usize - 2;
            if let Some(row) = reference.data.get(start..start + w + 5) {
                src[r][..w + 5].copy_from_slice(row);
                continue;
            }
        }
        for c in 0..w + 5 {
            src[r][c] = reference.at(x0 + c as isize - 2, yy);
        }
    }
    if fx == 0 && fy == 0 {
        for r in 0..h {
            let start = (by + r) * dst_stride + bx;
            if let Some(row) = dst.get_mut(start..start + w) {
                row.copy_from_slice(&src[r + 2][2..2 + w]);
            }
        }
        return;
    }
    let tap6 = |f: usize| -> [i32; 6] { std::array::from_fn(|k| filters.get(f * 6 + k).copied().unwrap_or(0)) };
    let (hf, vf) = (tap6(fx), tap6(fy));
    let round = |v: i32| ((v + 64) >> 7).clamp(0, 255);
    // horizontal pass over the h + 5 rows (an identity when fx is 0), then vertical
    let mut tmp = [[0i32; 16]; MAX];
    for r in 0..h + 5 {
        let s = &src[r];
        let t = &mut tmp[r];
        if fx == 0 {
            for c in 0..w {
                t[c] = i32::from(s[c + 2]);
            }
        } else {
            for c in 0..w {
                let v = i32::from(s[c]) * hf[0]
                    + i32::from(s[c + 1]) * hf[1]
                    + i32::from(s[c + 2]) * hf[2]
                    + i32::from(s[c + 3]) * hf[3]
                    + i32::from(s[c + 4]) * hf[4]
                    + i32::from(s[c + 5]) * hf[5];
                t[c] = round(v);
            }
        }
    }
    for r in 0..h {
        let start = (by + r) * dst_stride + bx;
        let Some(row) = dst.get_mut(start..start + w) else { continue };
        if fy == 0 {
            for (c, d) in row.iter_mut().enumerate() {
                *d = tmp[r + 2][c] as u8;
            }
        } else {
            for (c, d) in row.iter_mut().enumerate() {
                let v =
                    tmp[r][c] * vf[0] + tmp[r + 1][c] * vf[1] + tmp[r + 2][c] * vf[2] + tmp[r + 3][c] * vf[3] + tmp[r + 4][c] * vf[4] + tmp[r + 5][c] * vf[5];
                *d = round(v) as u8;
            }
        }
    }
}
