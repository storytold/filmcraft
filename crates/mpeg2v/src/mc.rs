//! Frame buffers and motion-compensated prediction (H.262 §7.6).

/// A decoded frame, padded to whole macroblocks.
#[derive(Clone)]
pub(crate) struct Frame {
    pub planes: [Vec<u8>; 3],
    /// Luma width / height (multiples of 16).
    pub w: usize,
    pub h: usize,
    /// Chroma width / height.
    pub cw: usize,
    pub ch: usize,
}

impl Frame {
    pub fn new(w: usize, h: usize, cw: usize, ch: usize) -> Frame {
        Frame { planes: [vec![16; w * h], vec![128; cw * ch], vec![128; cw * ch]], w, h, cw, ch }
    }

    /// Plane `c` (0 Y, 1 Cb, 2 Cr) as a reference: the whole frame, or one field (0 top, 1 bottom).
    #[inline]
    pub fn plane(&self, c: usize, field: Option<usize>) -> RefPlane<'_> {
        let (w, h) = if c == 0 { (self.w, self.h) } else { (self.cw, self.ch) };
        match field {
            None => RefPlane { data: &self.planes[c], off: 0, stride: w, w, h },
            Some(p) => RefPlane { data: &self.planes[c], off: p * w, stride: 2 * w, w, h: h / 2 },
        }
    }
}

/// A plane (or a field of one) to predict from.
#[derive(Clone, Copy)]
pub(crate) struct RefPlane<'a> {
    pub data: &'a [u8],
    pub off: usize,
    pub stride: usize,
    pub w: usize,
    pub h: usize,
}

/// A block to predict: integer position, half-sample flags and size.
#[derive(Clone, Copy)]
pub(crate) struct Block {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) hx: bool,
    pub(crate) hy: bool,
    pub(crate) bw: usize,
    pub(crate) bh: usize,
}

/// Form the `bw`×`bh` prediction at integer position (`x`, `y`) with half-sample flags into
/// `dst` (row `r` at `dst[doff + r·dstride..]`), or average it with what is there (`avg`).
/// Positions outside the reference are clamped to its edges (not allowed in conforming streams;
/// this keeps corrupt ones safe).
#[inline]
pub(crate) fn predict(dst: &mut [u8], doff: usize, dstride: usize, src: RefPlane, blk: Block, avg: bool) {
    let Block { x, y, hx, hy, bw, bh } = blk;
    let (ex, ey) = (hx as i32, hy as i32);
    let inside = x >= 0 && y >= 0 && (x + bw as i32 + ex) as usize <= src.w && (y + bh as i32 + ey) as usize <= src.h;
    let mut tmp = [0u8; 16];
    if inside {
        let s = src.stride;
        let base = src.off + y as usize * s + x as usize;
        for r in 0..bh {
            let p = base + r * s;
            let a = &src.data[p..p + bw + 1 - (!hx) as usize];
            let row = &mut tmp[..bw];
            match (hx, hy) {
                (false, false) => row.copy_from_slice(&a[..bw]),
                (true, false) => {
                    for i in 0..bw {
                        row[i] = ((a[i] as u16 + a[i + 1] as u16 + 1) >> 1) as u8;
                    }
                }
                (false, true) => {
                    let b = &src.data[p + s..p + s + bw];
                    for i in 0..bw {
                        row[i] = ((a[i] as u16 + b[i] as u16 + 1) >> 1) as u8;
                    }
                }
                (true, true) => {
                    let b = &src.data[p + s..p + s + bw + 1];
                    for i in 0..bw {
                        row[i] = ((a[i] as u16 + a[i + 1] as u16 + b[i] as u16 + b[i + 1] as u16 + 2) >> 2) as u8;
                    }
                }
            }
            store(dst, doff + r * dstride, row, avg);
        }
        return;
    }
    let get = |xx: i32, yy: i32| -> u16 {
        let xx = xx.clamp(0, src.w as i32 - 1) as usize;
        let yy = yy.clamp(0, src.h as i32 - 1) as usize;
        src.data[src.off + yy * src.stride + xx] as u16
    };
    for r in 0..bh {
        let yy = y + r as i32;
        let row = &mut tmp[..bw];
        for (i, v) in row.iter_mut().enumerate() {
            let xx = x + i as i32;
            *v = match (hx, hy) {
                (false, false) => get(xx, yy),
                (true, false) => (get(xx, yy) + get(xx + 1, yy) + 1) >> 1,
                (false, true) => (get(xx, yy) + get(xx, yy + 1) + 1) >> 1,
                (true, true) => (get(xx, yy) + get(xx + 1, yy) + get(xx, yy + 1) + get(xx + 1, yy + 1) + 2) >> 2,
            } as u8;
        }
        store(dst, doff + r * dstride, row, avg);
    }
}

#[inline(always)]
fn store(dst: &mut [u8], at: usize, row: &[u8], avg: bool) {
    let d = &mut dst[at..at + row.len()];
    if avg {
        for (o, &p) in d.iter_mut().zip(row) {
            *o = ((*o as u16 + p as u16 + 1) >> 1) as u8;
        }
    } else {
        d.copy_from_slice(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        let mut f = Frame::new(32, 32, 16, 16);
        for (i, v) in f.planes[0].iter_mut().enumerate() {
            *v = (i % 32 * 3 + i / 32 * 5) as u8;
        }
        f
    }

    #[test]
    fn half_sample_interpolation_rounds_up() {
        let f = frame();
        let p = f.plane(0, None);
        let mut d = [0u8; 4];
        predict(&mut d, 0, 2, p, Block { x: 1, y: 1, hx: true, hy: true, bw: 2, bh: 2 }, false);
        let s = |x: usize, y: usize| f.planes[0][y * 32 + x] as u16;
        assert_eq!(d[0] as u16, (s(1, 1) + s(2, 1) + s(1, 2) + s(2, 2) + 2) >> 2);
        predict(&mut d, 0, 2, p, Block { x: 1, y: 1, hx: true, hy: false, bw: 2, bh: 2 }, false);
        assert_eq!(d[3] as u16, (s(2, 2) + s(3, 2) + 1) >> 1);
    }

    #[test]
    fn fields_and_clamped_edges() {
        let f = frame();
        let mut d = [0u8; 4];
        // bottom field row 0 = frame row 1
        predict(&mut d, 0, 2, f.plane(0, Some(1)), Block { x: 0, y: 0, hx: false, hy: false, bw: 2, bh: 2 }, false);
        assert_eq!(d, [f.planes[0][32], f.planes[0][33], f.planes[0][96], f.planes[0][97]]);
        // outside: clamped to the corner
        predict(&mut d, 0, 2, f.plane(0, None), Block { x: -10, y: -10, hx: false, hy: false, bw: 2, bh: 2 }, false);
        assert_eq!(d, [f.planes[0][0]; 4]);
        // averaging
        let mut d = [10u8; 4];
        predict(&mut d, 0, 2, f.plane(0, None), Block { x: -10, y: -10, hx: false, hy: false, bw: 2, bh: 2 }, true);
        assert_eq!(d[0], ((10 + f.planes[0][0] as u16 + 1) >> 1) as u8);
    }
}
