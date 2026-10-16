//! Padded picture planes, reference pictures and motion-compensated prediction (§8.4.2.2).

use crate::mbinfo::{MbInfo, Mv};
use crate::pixel::{Depth, Pixel};

pub const LUMA_PAD: usize = 64;
pub const CHROMA_PAD: usize = 32;

/// A plane with `pad` replicated samples on every side.
#[derive(Clone)]
pub struct Plane<P> {
    pub data: Vec<P>,
    pub stride: usize,
    pub w: usize,
    pub h: usize,
    pub pad: usize,
}

impl<P: Pixel> Plane<P> {
    pub fn new(w: usize, h: usize, pad: usize) -> Self {
        let stride = w + 2 * pad;
        Plane { data: vec![P::default(); stride * (h + 2 * pad)], stride, w, h, pad }
    }
    #[inline]
    pub fn idx(&self, x: isize, y: isize) -> usize {
        ((y + self.pad as isize) as usize) * self.stride + (x + self.pad as isize) as usize
    }
    /// Replicate edge samples into the padding.
    pub fn extend_edges(&mut self) {
        let (w, h, pad, stride) = (self.w, self.h, self.pad, self.stride);
        for y in 0..h {
            let r = (y + pad) * stride;
            let l = self.data[r + pad];
            let rr = self.data[r + pad + w - 1];
            self.data[r..r + pad].fill(l);
            self.data[r + pad + w..r + stride].fill(rr);
        }
        let first = pad * stride;
        let last = (pad + h - 1) * stride;
        for y in 0..pad {
            self.data.copy_within(first..first + stride, y * stride);
            self.data.copy_within(last..last + stride, (pad + h + y) * stride);
        }
    }
    /// Copy the picture area out as a tightly packed plane of `cw x ch` (cropping), as bytes ([`Pixel::extend_bytes`]).
    pub fn copy_out(&self, cw: usize, ch: usize, out: &mut Vec<u8>) {
        for y in 0..ch {
            let o = self.idx(0, y as isize);
            P::extend_bytes(&self.data[o..o + cw], out);
        }
    }
}

/// A 4:2:0 frame with padded planes.
#[derive(Clone)]
pub struct Frame<P> {
    pub y: Plane<P>,
    pub u: Plane<P>,
    pub v: Plane<P>,
}

impl<P: Pixel> Frame<P> {
    pub fn new(w: usize, h: usize) -> Self {
        Frame { y: Plane::new(w, h, LUMA_PAD), u: Plane::new(w / 2, h / 2, CHROMA_PAD), v: Plane::new(w / 2, h / 2, CHROMA_PAD) }
    }
    pub fn extend_edges(&mut self) {
        self.y.extend_edges();
        self.u.extend_edges();
        self.v.extend_edges();
    }
}

/// A decoded reference picture with half-sample planes for fast sub-pel access.
pub struct RefPic<P> {
    pub frame: Frame<P>,
    /// Horizontal half (b), vertical half (h) and centre (j) planes, same geometry as luma.
    pub hpel: [Plane<P>; 3],
    pub mbs: Vec<MbInfo>,
    pub poc: i32,
    pub id: u32,
}

/// Compute the H, V and centre half-sample planes over the whole padded area (clamped at the buffer edge,
/// which equals the normative picture-edge clamping because the padding is replicated).
pub fn build_hpel<P: Pixel>(f: &Plane<P>, d: Depth, reuse: Option<[Plane<P>; 3]>) -> [Plane<P>; 3] {
    let [mut hp, mut vp, mut cp] = reuse.filter(|r| r[0].data.len() == f.data.len()).unwrap_or_else(|| {
        let mk = || Plane { data: vec![P::default(); f.data.len()], stride: f.stride, w: f.w, h: f.h, pad: f.pad };
        [mk(), mk(), mk()]
    });
    let rows = f.data.len() / f.stride;
    let band = 16usize;
    let work = |b: usize, hrows: &mut [P], vrows: &mut [P], crows: &mut [P]| {
        let r0 = b * band;
        let r1 = (r0 + band).min(rows);
        hpel_rows(f, d, r0, r1, hrows, vrows, crows);
    };
    let chunk = band * f.stride;
    #[cfg(feature = "threads")]
    {
        use rayon::prelude::*;
        hp.data
            .par_chunks_mut(chunk)
            .zip(vp.data.par_chunks_mut(chunk))
            .zip(cp.data.par_chunks_mut(chunk))
            .enumerate()
            .for_each(|(b, ((h, v), c))| work(b, h, v, c));
    }
    #[cfg(not(feature = "threads"))]
    {
        for (b, ((h, v), c)) in hp.data.chunks_mut(chunk).zip(vp.data.chunks_mut(chunk)).zip(cp.data.chunks_mut(chunk)).enumerate() {
            work(b, h, v, c);
        }
    }
    [hp, vp, cp]
}

fn hpel_rows<P: Pixel>(f: &Plane<P>, d: Depth, r0: usize, r1: usize, hout: &mut [P], vout: &mut [P], cout: &mut [P]) {
    let stride = f.stride;
    let rows = f.data.len() / stride;
    let clampr = |r: isize| r.clamp(0, rows as isize - 1) as usize;
    // Horizontal intermediate (unclipped, unshifted) for rows r0-2 .. r1+3.
    let nrows = r1 - r0 + 5;
    let mut hs = vec![P::Tap::default(); nrows * stride];
    let mut ext = vec![P::default(); stride + 5];
    for k in 0..nrows {
        let r = clampr(r0 as isize - 2 + k as isize);
        let row = &f.data[r * stride..r * stride + stride];
        ext[0] = row[0];
        ext[1] = row[0];
        ext[2..2 + stride].copy_from_slice(row);
        ext[stride + 2] = row[stride - 1];
        ext[stride + 3] = row[stride - 1];
        ext[stride + 4] = row[stride - 1];
        let o = &mut hs[k * stride..k * stride + stride];
        for (o, e) in o.iter_mut().zip(ext.windows(6)) {
            if let Some(e) = e.first_chunk::<6>() {
                *o = P::tap6(e);
            }
        }
    }
    for r in r0..r1 {
        let lo = (r - r0) * stride;
        let k = r - r0 + 2;
        let hrow = &hs[k * stride..k * stride + stride];
        for x in 0..stride {
            hout[lo + x] = P::clip((P::untap(hrow[x]) + 16) >> 5, d);
        }
        // vertical
        let rr: [usize; 6] = std::array::from_fn(|i| clampr(r as isize - 2 + i as isize) * stride);
        for x in 0..stride {
            let px = |i: usize| f.data[rr[i] + x].get();
            let s = px(0) + px(5) - 5 * (px(1) + px(4)) + 20 * (px(2) + px(3));
            vout[lo + x] = P::clip((s + 16) >> 5, d);
        }
        // centre from horizontal intermediates
        let hk: [&[P::Tap]; 6] = std::array::from_fn(|i| &hs[(k - 2 + i) * stride..(k - 1 + i) * stride]);
        for x in 0..stride {
            let t = |i: usize| P::untap(hk[i][x]);
            let s = t(0) + t(5) - 5 * (t(1) + t(4)) + 20 * (t(2) + t(3));
            cout[lo + x] = P::clip((s + 512) >> 10, d);
        }
    }
}

/// Plane selection per quarter-sample position: two (plane, dx, dy) sources to average (identical = no average).
/// Plane indices: 0 full, 1 horizontal half, 2 vertical half, 3 centre.
const QPEL_SRC: [[(u8, u8, u8); 2]; 16] = [
    // fy = 0
    [(0, 0, 0), (0, 0, 0)],
    [(0, 0, 0), (1, 0, 0)],
    [(1, 0, 0), (1, 0, 0)],
    [(1, 0, 0), (0, 1, 0)],
    // fy = 1
    [(0, 0, 0), (2, 0, 0)],
    [(1, 0, 0), (2, 0, 0)],
    [(1, 0, 0), (3, 0, 0)],
    [(1, 0, 0), (2, 1, 0)],
    // fy = 2
    [(2, 0, 0), (2, 0, 0)],
    [(2, 0, 0), (3, 0, 0)],
    [(3, 0, 0), (3, 0, 0)],
    [(3, 0, 0), (2, 1, 0)],
    // fy = 3
    [(2, 0, 0), (0, 0, 1)],
    [(2, 0, 0), (1, 0, 1)],
    [(3, 0, 0), (1, 0, 1)],
    [(2, 1, 0), (1, 0, 1)],
];

impl<P: Pixel> RefPic<P> {
    #[inline]
    fn plane(&self, p: u8) -> &Plane<P> {
        match p {
            0 => &self.frame.y,
            n => &self.hpel[n as usize - 1],
        }
    }

    /// Luma prediction of a w x h block at picture position (x, y) displaced by `mv` (quarter samples).
    pub fn mc_luma(&self, x: usize, y: usize, mv: Mv, w: usize, h: usize, out: &mut [P], ostride: usize) {
        let xi = x as isize + (mv.x as isize >> 2);
        let yi = y as isize + (mv.y as isize >> 2);
        let q = ((mv.y & 3) * 4 + (mv.x & 3)) as usize;
        let [(pa, dxa, dya), (pb, dxb, dyb)] = QPEL_SRC[q];
        let a = self.plane(pa);
        let stride = a.stride;
        let ia = a.idx(xi + dxa as isize, yi + dya as isize);
        if pa == pb && dxa == dxb && dya == dyb {
            for r in 0..h {
                out[r * ostride..r * ostride + w].copy_from_slice(&a.data[ia + r * stride..ia + r * stride + w]);
            }
        } else {
            let b = self.plane(pb);
            let ib = b.idx(xi + dxb as isize, yi + dyb as isize);
            for r in 0..h {
                let ra = &a.data[ia + r * stride..ia + r * stride + w];
                let rb = &b.data[ib + r * stride..ib + r * stride + w];
                let o = &mut out[r * ostride..r * ostride + w];
                for i in 0..w {
                    o[i] = ra[i].avg(rb[i]);
                }
            }
        }
    }

    /// Chroma prediction (both planes) of a w x h chroma block at chroma position (x, y); mv in luma quarter units.
    #[allow(clippy::too_many_arguments)]
    pub fn mc_chroma(&self, x: usize, y: usize, mv: Mv, w: usize, h: usize, out_u: &mut [P], out_v: &mut [P], ostride: usize) {
        let xi = x as isize + (mv.x as isize >> 3);
        let yi = y as isize + (mv.y as isize >> 3);
        let fx = (mv.x & 7) as i32;
        let fy = (mv.y & 7) as i32;
        let w00 = (8 - fx) * (8 - fy);
        let w01 = fx * (8 - fy);
        let w10 = (8 - fx) * fy;
        let w11 = fx * fy;
        for (p, out) in [(&self.frame.u, out_u), (&self.frame.v, out_v)] {
            let s = p.stride;
            let i0 = p.idx(xi, yi);
            for r in 0..h {
                let a = &p.data[i0 + r * s..i0 + r * s + w + 1];
                let b = &p.data[i0 + (r + 1) * s..i0 + (r + 1) * s + w + 1];
                let o = &mut out[r * ostride..r * ostride + w];
                for i in 0..w {
                    o[i] = P::of((w00 * a[i].get() + w01 * a[i + 1].get() + w10 * b[i].get() + w11 * b[i + 1].get() + 32) >> 6);
                }
            }
        }
    }
}

/// Average two predictions in place (default bi-prediction).
#[inline]
pub fn avg_into<P: Pixel>(dst: &mut [P], src: &[P]) {
    for (d, &s) in dst.iter_mut().zip(src) {
        *d = d.avg(s);
    }
}
