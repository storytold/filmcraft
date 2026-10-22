//! Padded picture planes, the coded frame and its single reference picture.
//!
//! Layout mirrors `h264enc`'s picture module but is simpler on purpose: phase 1 interpolates each inter block directly
//! from these planes with the spec §8.5.3.2 8-tap luma / 4-tap chroma filters (`tables::LUMA_FILTER` / `CHROMA_FILTER`).
//! Precomputed half-pel planes like `h264enc`'s `build_hpel` are a follow-up; motion compensation lives in `inter.rs`
//! and operates on `&Plane`, not here.

use crate::Error;

/// One 8-bit plane with `pad` replicated samples on every side (`pad` is derived, not stored: `pad = (stride - w) / 2`).
#[derive(Clone, Debug)]
pub struct Plane {
    pub data: Vec<u8>,
    pub stride: usize,
    pub w: usize,
    pub h: usize,
}

impl Plane {
    /// Blank plane: `stride = w + 2 * pad`, zeroed data. Saturating math keeps hostile sizes from wrapping.
    pub fn new(w: usize, h: usize, pad: usize) -> Self {
        let stride = w.saturating_add(pad.saturating_mul(2));
        let rows = h.saturating_add(pad.saturating_mul(2));
        Plane { data: vec![0; stride.saturating_mul(rows)], stride, w, h }
    }

    /// Padding derived from the stride (see [`Plane::new`]).
    fn pad(&self) -> usize {
        self.stride.saturating_sub(self.w) / 2
    }

    /// Index of the visible sample (x, y); the pad offset is included. Callers hold x < w and y < h.
    #[inline]
    pub fn idx(&self, x: usize, y: usize) -> usize {
        y.saturating_add(self.pad()).saturating_mul(self.stride).saturating_add(x.saturating_add(self.pad()))
    }

    /// Set one visible sample; out-of-range writes are ignored, never panic.
    /// This sidesteps the borrow checker where callers stage predictions into recon planes.
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: u8) {
        if x < self.w && y < self.h {
            let pad = self.pad();
            if let Some(s) = self.data.get_mut(y.saturating_add(pad).saturating_mul(self.stride).saturating_add(x.saturating_add(pad))) {
                *s = v;
            }
        }
    }

    /// Replicate the visible edges into all padding (short/degenerate buffers return unmodified, never panic).
    pub fn extend_edges(&mut self) {
        let pad = self.pad();
        let (w, h, stride) = (self.w, self.h, self.stride);
        if pad == 0 || w == 0 || h == 0 || stride < w.saturating_add(pad) {
            return;
        }
        let rows = match h.checked_add(pad.saturating_mul(2)) {
            Some(r) => r,
            None => return,
        };
        if self.data.len() < stride.saturating_mul(rows) {
            return;
        }
        for y in 0..h {
            let r = y.saturating_add(pad).saturating_mul(stride);
            let l = self.data[r.saturating_add(pad)];
            let rr = self.data[r.saturating_add(pad).saturating_add(w).saturating_sub(1)];
            self.data[r..r.saturating_add(pad)].fill(l);
            self.data[r.saturating_add(pad).saturating_add(w)..r.saturating_add(stride)].fill(rr);
        }
        let first = pad.saturating_mul(stride);
        let last = pad.saturating_add(h).saturating_sub(1).saturating_mul(stride);
        for y in 0..pad {
            self.data.copy_within(first..first.saturating_add(stride), y.saturating_mul(stride));
            self.data.copy_within(last..last.saturating_add(stride), pad.saturating_add(h).saturating_add(y).saturating_mul(stride));
        }
    }

    /// Append the visible `w x h` samples row by row, without stride. Short buffers stop early, never panic.
    pub fn copy_out(&self, out: &mut Vec<u8>) {
        out.reserve(self.w.saturating_mul(self.h));
        let pad = self.pad();
        for y in 0..self.h {
            let o = y.saturating_add(pad).saturating_mul(self.stride).saturating_add(pad);
            if let Some(row) = self.data.get(o..o.saturating_add(self.w)) {
                out.extend_from_slice(row);
            } else {
                break;
            }
        }
    }
}

/// One coded 4:2:0 picture: full-size luma plus half-size chroma with the same padding.
#[derive(Clone, Debug)]
pub struct Frame {
    pub y: Plane,
    pub u: Plane,
    pub v: Plane,
}

impl Frame {
    /// Coded-size frame; chroma planes are half width/height with the same `pad`.
    pub fn new(coded_w: usize, coded_h: usize, pad: usize) -> Self {
        Frame { y: Plane::new(coded_w, coded_h, pad), u: Plane::new(coded_w / 2, coded_h / 2, pad), v: Plane::new(coded_w / 2, coded_h / 2, pad) }
    }
}

/// Phase-1 reference: the single previous reconstructed picture (`inter.rs` reads its planes directly).
#[derive(Clone, Debug)]
pub struct RefPic {
    pub frame: Frame,
}

/// Copy one strided source plane into `dst`, filling rows/columns past the visible area (never panics).
fn copy_plane(dst: &mut Plane, src: &[u8], stride: usize, pw: usize, ph: usize) -> Result<(), Error> {
    if dst.w < pw || dst.h < ph {
        return Err(Error::BadFrame);
    }
    let pad = dst.pad();
    for y in 0..dst.h {
        let sy = y.min(ph.saturating_sub(1)); // bottom overhang repeats the last source row
        let s = sy.saturating_mul(stride);
        let o = y.saturating_add(pad).saturating_mul(dst.stride).saturating_add(pad);
        let row = src.get(s..s.saturating_add(pw)).ok_or(Error::BadFrame)?;
        let drow = dst.data.get_mut(o..o.saturating_add(pw)).ok_or(Error::BadFrame)?;
        drow.copy_from_slice(row);
        let last = row.last().copied().ok_or(Error::BadFrame)?; // right overhang repeats the last sample
        if let Some(fill) = dst.data.get_mut(o.saturating_add(pw)..o.saturating_add(dst.w)) {
            fill.fill(last);
        } else {
            return Err(Error::BadFrame);
        }
    }
    Ok(())
}

/// Load a strided 4:2:0 input picture into `into`, replicating its right/bottom edge over any coded-size overhang
/// (`into` may equal the coded size or a larger CTU-rounded frame). Short planes or strides fail with `BadFrame`.
/// Padding for sub-pel filtering is left to [`Plane::extend_edges`], which callers run before motion compensation.
pub fn import_yuv(y: &[u8], u: &[u8], v: &[u8], y_stride: usize, uv_stride: usize, coded_w: usize, coded_h: usize, into: &mut Frame) -> Result<(), Error> {
    if coded_w == 0 || coded_h == 0 || !coded_w.is_multiple_of(2) || !coded_h.is_multiple_of(2) {
        return Err(Error::BadFrame);
    }
    let (cw, ch) = (coded_w / 2, coded_h / 2);
    if y_stride < coded_w || uv_stride < cw {
        return Err(Error::BadFrame);
    }
    let need_y = y_stride.checked_mul(coded_h - 1).and_then(|n| n.checked_add(coded_w)).ok_or(Error::BadFrame)?;
    let need_c = uv_stride.checked_mul(ch - 1).and_then(|n| n.checked_add(cw)).ok_or(Error::BadFrame)?;
    if y.len() < need_y || u.len() < need_c || v.len() < need_c {
        return Err(Error::BadFrame);
    }
    copy_plane(&mut into.y, y, y_stride, coded_w, coded_h)?;
    copy_plane(&mut into.u, u, uv_stride, cw, ch)?;
    copy_plane(&mut into.v, v, uv_stride, cw, ch)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_geometry_counts_pad_on_every_side() {
        let p = Plane::new(4, 3, 2);
        assert_eq!((p.w, p.h, p.stride), (4, 3, 8));
        assert_eq!(p.data.len(), 8 * 7);
    }

    #[test]
    fn idx_counts_the_pad_offset() {
        let p = Plane::new(4, 3, 2);
        assert_eq!(p.idx(0, 0), 2 * 8 + 2);
        assert_eq!(p.idx(3, 2), 4 * 8 + 5);
    }

    #[test]
    fn extend_edges_replicates_sides_and_corners() {
        let mut p = Plane::new(2, 2, 1);
        for (x, y, v) in [(0, 0, 10u8), (1, 0, 20), (0, 1, 30), (1, 1, 40)] {
            let o = p.idx(x, y);
            p.data[o] = v;
        }
        p.extend_edges();
        assert_eq!(p.data, vec![10, 10, 20, 20, 10, 10, 20, 20, 30, 30, 40, 40, 30, 30, 40, 40]);
    }

    #[test]
    fn copy_out_round_trips_visible_samples() {
        let mut p = Plane::new(3, 2, 2);
        for y in 0..2 {
            for x in 0..3 {
                let o = p.idx(x, y);
                p.data[o] = (y * 3 + x + 1) as u8;
            }
        }
        p.extend_edges();
        let mut out = Vec::new();
        p.copy_out(&mut out);
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn frame_halves_chroma_with_the_same_pad() {
        let f = Frame::new(8, 6, 2);
        assert_eq!((f.y.w, f.y.h, f.y.stride), (8, 6, 12));
        assert_eq!((f.u.w, f.u.h, f.u.stride), (4, 3, 8));
        assert_eq!((f.v.w, f.v.h, f.v.stride), (4, 3, 8));
        assert_eq!(f.y.data.len(), 12 * 10);
        assert_eq!(f.u.data.len(), 8 * 7);
        assert_eq!(f.v.data.len(), 8 * 7);
    }

    #[test]
    fn import_copies_strided_input_exactly() {
        let mut f = Frame::new(4, 2, 1);
        let y = vec![1u8, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0]; // 4x2 visible in a stride-6 layout
        import_yuv(&y, &[11u8, 12], &[21u8, 22], 6, 2, 4, 2, &mut f).unwrap();
        let mut out = Vec::new();
        f.y.copy_out(&mut out);
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let mut out_u = Vec::new();
        f.u.copy_out(&mut out_u);
        assert_eq!(out_u, vec![11, 12]);
    }

    #[test]
    fn import_replicates_right_and_bottom_edges_into_the_overhang() {
        let mut f = Frame::new(8, 8, 1);
        let mut y = vec![0u8; 16]; // 4x4 visible, value 10*x + y so every edge is distinguishable
        for yy in 0..4 {
            for xx in 0..4 {
                y[yy * 4 + xx] = (10 * xx + yy) as u8;
            }
        }
        import_yuv(&y, &[7u8; 4], &[9u8; 4], 4, 2, 4, 4, &mut f).unwrap();
        let at = |p: &Plane, x: usize, y: usize| p.data[p.idx(x, y)];
        assert_eq!(at(&f.y, 0, 0), 0);
        assert_eq!(at(&f.y, 3, 3), 33);
        assert_eq!(at(&f.y, 7, 0), 30);
        assert_eq!(at(&f.y, 0, 7), 3);
        assert_eq!(at(&f.y, 7, 7), 33);
        assert_eq!(at(&f.u, 3, 3), 7);
        assert_eq!(at(&f.v, 3, 0), 9);
    }

    #[test]
    fn import_rejects_short_planes_and_strides() {
        let mut f = Frame::new(8, 8, 1);
        let y = vec![0u8; 8 * 8];
        let u = vec![0u8; 4 * 4];
        let v = vec![0u8; 4 * 4];
        assert!(import_yuv(&y, &u, &v, 8, 3, 8, 8, &mut f).is_err()); // uv stride narrower than coded/2
        assert!(import_yuv(&y, &u, &v, 7, 4, 8, 8, &mut f).is_err()); // luma stride narrower than coded
        assert!(import_yuv(&[0u8; 8 * 7 + 7], &u, &v, 8, 4, 8, 8, &mut f).is_err()); // truncated luma
        assert!(import_yuv(&y, &[0u8; 4 * 3 + 3], &v, 8, 4, 8, 8, &mut f).is_err()); // truncated chroma
        assert!(import_yuv(&y, &u, &v, 8, 4, 7, 8, &mut f).is_err()); // odd coded width
    }
}
