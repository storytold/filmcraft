//! Frame buffers and motion-compensated prediction (ISO/IEC 14496-2 §7.6): half-sample bilinear
//! and quarter-sample 8-tap interpolation with unrestricted vectors. Reference samples outside the
//! frame are those of its edge (the padding of §7.6.4, here taken by clamping coordinates).

/// One plane; rows are `w` samples apart.
#[derive(Clone, Copy)]
pub(crate) struct Plane<'a> {
    pub data: &'a [u8],
    pub w: usize,
    pub h: usize,
}

/// A decoded frame, padded to whole macroblocks.
#[derive(Clone, Default)]
pub(crate) struct Frame {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    /// Luma width / height (multiples of 16).
    pub w: usize,
    pub h: usize,
}

impl Frame {
    pub fn new(mb_w: usize, mb_h: usize) -> Frame {
        let (w, h) = (mb_w * 16, mb_h * 16);
        Frame { y: vec![16; w * h], u: vec![128; w * h / 4], v: vec![128; w * h / 4], w, h }
    }
    #[inline]
    pub fn luma(&self) -> Plane<'_> {
        Plane { data: &self.y, w: self.w, h: self.h }
    }
    #[inline]
    pub fn cb(&self) -> Plane<'_> {
        Plane { data: &self.u, w: self.w / 2, h: self.h / 2 }
    }
    #[inline]
    pub fn cr(&self) -> Plane<'_> {
        Plane { data: &self.v, w: self.w / 2, h: self.h / 2 }
    }
}

/// Largest block with its extra row and column, rows `TMP` apart.
const TMP: usize = 17;

/// The `bw`×`bh` samples from (`x`, `y`): in the plane when inside it, else in `tmp` with
/// coordinates clamped to the edges. Returns the samples and their stride.
#[inline]
fn region<'a>(p: &Plane<'a>, x: i32, y: i32, bw: usize, bh: usize, tmp: &'a mut [u8; TMP * TMP]) -> (&'a [u8], usize) {
    if x >= 0 && y >= 0 {
        let (ux, uy) = (x as usize, y as usize);
        if ux + bw <= p.w
            && uy + bh <= p.h
            && let Some(s) = p.data.get(uy * p.w + ux..)
        {
            return (s, p.w);
        }
    }
    let (mx, my) = (p.w as i32 - 1, p.h as i32 - 1);
    for (r, row) in tmp.as_chunks_mut::<TMP>().0.iter_mut().take(bh).enumerate() {
        let yy = (y.saturating_add(r as i32)).clamp(0, my) as usize;
        let Some(src) = p.data.get(yy * p.w..) else { continue };
        for (c, o) in row.iter_mut().take(bw).enumerate() {
            let xx = (x.saturating_add(c as i32)).clamp(0, mx) as usize;
            *o = src.get(xx).copied().unwrap_or(128);
        }
    }
    (&tmp[..], TMP)
}

/// Half-sample prediction of a `W`×`W` block (§7.6.2) at (`x`, `y`) displaced by (`mx`, `my`) half
/// samples, into `d` (rows `ds` apart).
#[inline]
pub(crate) fn half<const W: usize>(d: &mut [u8], ds: usize, p: &Plane, (x, y): (i32, i32), (mx, my): (i32, i32), rounding: u8) {
    let (fx, fy) = (mx & 1 != 0, my & 1 != 0);
    let (ix, iy) = (x + (mx >> 1), y + (my >> 1));
    let mut tmp = [0u8; TMP * TMP];
    let (s, ss) = region(p, ix, iy, W + fx as usize, W + fy as usize, &mut tmp);
    let rnd = u16::from(rounding);
    for (r, row) in d.chunks_mut(ds).take(W).enumerate() {
        let Some(o) = row.first_chunk_mut::<W>() else { return };
        let at = r * ss;
        let Some(a) = s.get(at..).and_then(|s| s.first_chunk::<W>()) else { return };
        match (fx, fy) {
            (false, false) => *o = *a,
            (true, false) => {
                let Some(b) = s.get(at + 1..).and_then(|s| s.first_chunk::<W>()) else { return };
                for i in 0..W {
                    o[i] = ((a[i] as u16 + b[i] as u16 + 1 - rnd) >> 1) as u8;
                }
            }
            (false, true) => {
                let Some(b) = s.get(at + ss..).and_then(|s| s.first_chunk::<W>()) else { return };
                for i in 0..W {
                    o[i] = ((a[i] as u16 + b[i] as u16 + 1 - rnd) >> 1) as u8;
                }
            }
            (true, true) => {
                let Some(b) = s.get(at + 1..).and_then(|s| s.first_chunk::<W>()) else { return };
                let Some(c) = s.get(at + ss..).and_then(|s| s.first_chunk::<W>()) else { return };
                let Some(e) = s.get(at + ss + 1..).and_then(|s| s.first_chunk::<W>()) else { return };
                for i in 0..W {
                    o[i] = ((a[i] as u16 + b[i] as u16 + c[i] as u16 + e[i] as u16 + 2 - rnd) >> 2) as u8;
                }
            }
        }
    }
}

/// The 8-tap half-sample filter at position `c` of `e`, which holds the block from sample -3.
#[inline(always)]
fn fir(e: &[i16], c: usize, rnd: i16) -> u8 {
    let t = |i: usize| i32::from(e.get(c + i).copied().unwrap_or(0));
    let v = 20 * (t(3) + t(4)) - 6 * (t(2) + t(5)) + 3 * (t(1) + t(6)) - (t(0) + t(7));
    ((v + 16 - i32::from(rnd)) >> 5).clamp(0, 255) as u8
}

/// `n` + 1 samples `s` extended past both ends by mirroring (§7.6.2.1) into `e`.
#[inline(always)]
fn mirror(s: &[u8], n: usize, e: &mut [i16; 24]) {
    let get = |i: usize| i16::from(s.get(i).copied().unwrap_or(0));
    e[0] = get(2);
    e[1] = get(1);
    e[2] = get(0);
    for i in 0..=n {
        if let Some(o) = e.get_mut(3 + i) {
            *o = get(i);
        }
    }
    for k in 0..3 {
        if let Some(o) = e.get_mut(n + 4 + k) {
            *o = get(n.saturating_sub(k));
        }
    }
}

/// Quarter-sample prediction of a `W`×`W` block (§7.6.2.1), `W` 8 or 16: the horizontal filter
/// over the block's rows, then the vertical one over its result, each averaged with the nearer
/// sample at the odd positions.
pub(crate) fn quarter<const W: usize>(d: &mut [u8], ds: usize, p: &Plane, (x, y): (i32, i32), (mx, my): (i32, i32), rounding: u8) {
    let (fx, fy) = ((mx & 3) as usize, (my & 3) as usize);
    let (ix, iy) = (x + (mx >> 2), y + (my >> 2));
    let mut tmp = [0u8; TMP * TMP];
    let (s, ss) = region(p, ix, iy, W + 1, W + 1, &mut tmp);
    let rnd = i16::from(rounding);
    let avg = |a: u8, b: u8| ((u16::from(a) + u16::from(b) + 1 - rnd as u16) >> 1) as u8;
    // horizontal stage, W + 1 rows
    let mut t = [[0u8; TMP]; TMP];
    let mut e = [0i16; 24];
    for (r, out) in t.iter_mut().take(W + 1).enumerate() {
        let Some(row) = s.get(r * ss..) else { break };
        if fx == 0 {
            for (o, v) in out.iter_mut().zip(row).take(W) {
                *o = *v;
            }
            continue;
        }
        mirror(row, W, &mut e);
        for c in 0..W {
            let h = fir(&e, c, rnd);
            let v = match fx {
                1 => avg(row.get(c).copied().unwrap_or(0), h),
                2 => h,
                _ => avg(row.get(c + 1).copied().unwrap_or(0), h),
            };
            if let Some(o) = out.get_mut(c) {
                *o = v;
            }
        }
    }
    // vertical stage
    let mut col = [0u8; TMP];
    for c in 0..W {
        if fy != 0 {
            for (o, row) in col.iter_mut().zip(t.iter()).take(W + 1) {
                *o = row.get(c).copied().unwrap_or(0);
            }
            mirror(&col, W, &mut e);
        }
        for r in 0..W {
            let a = t.get(r).and_then(|t| t.get(c)).copied().unwrap_or(0);
            let v = match fy {
                0 => a,
                2 => fir(&e, r, rnd),
                1 => avg(a, fir(&e, r, rnd)),
                _ => avg(col.get(r + 1).copied().unwrap_or(0), fir(&e, r, rnd)),
            };
            if let Some(o) = d.get_mut(r * ds + c) {
                *o = v;
            }
        }
    }
}

/// The destination of a macroblock's prediction: slices that start at its first sample.
pub(crate) struct Out<'a> {
    pub y: &'a mut [u8],
    pub u: &'a mut [u8],
    pub v: &'a mut [u8],
    pub ys: usize,
    pub cs: usize,
}

/// Chroma vector of a macroblock with one luma vector (half samples; §7.6.3).
#[inline]
fn chroma_one(v: i32, quarter: bool) -> i32 {
    let m = if quarter { v / 2 } else { v };
    (m >> 1) | (m & 1)
}

/// Table 7-9: rounding of the sixteenth-sample chroma position.
const ROUND16: [i32; 16] = [0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2];

/// Chroma vector from the sum of four luma vectors.
#[inline]
fn chroma_four(sum: i32) -> i32 {
    (sum >> 4) * 2 + ROUND16.get((sum & 15) as usize).copied().unwrap_or(0)
}

/// How a macroblock is predicted from one reference.
#[derive(Clone, Copy)]
pub(crate) struct Mc {
    pub rounding: u8,
    pub quarter: bool,
}

impl Mc {
    #[inline]
    fn chroma(&self, o: &mut Out, r: &Frame, x: usize, y: usize, cx: i32, cy: i32) {
        let (px, py) = ((x * 8) as i32, (y * 8) as i32);
        half::<8>(o.u, o.cs, &r.cb(), (px, py), (cx, cy), self.rounding);
        half::<8>(o.v, o.cs, &r.cr(), (px, py), (cx, cy), self.rounding);
    }

    /// Macroblock (`x`, `y`) from one vector.
    pub fn one(&self, o: &mut Out, r: &Frame, x: usize, y: usize, v: (i32, i32)) {
        let (px, py) = ((x * 16) as i32, (y * 16) as i32);
        if self.quarter {
            quarter::<16>(o.y, o.ys, &r.luma(), (px, py), (v.0, v.1), self.rounding);
        } else {
            half::<16>(o.y, o.ys, &r.luma(), (px, py), (v.0, v.1), self.rounding);
        }
        self.chroma(o, r, x, y, chroma_one(v.0, self.quarter), chroma_one(v.1, self.quarter));
    }

    /// Macroblock (`x`, `y`) from four vectors.
    pub fn four(&self, o: &mut Out, r: &Frame, x: usize, y: usize, vs: &[(i32, i32); 4]) {
        let (px, py) = ((x * 16) as i32, (y * 16) as i32);
        let (mut sx, mut sy) = (0, 0);
        for (k, v) in vs.iter().enumerate() {
            let (bx, by) = (k & 1, k >> 1);
            let Some(d) = o.y.get_mut(by * 8 * o.ys + bx * 8..) else { return };
            let (qx, qy) = (px + (bx * 8) as i32, py + (by * 8) as i32);
            if self.quarter {
                quarter::<8>(d, o.ys, &r.luma(), (qx, qy), (v.0, v.1), self.rounding);
                sx += v.0 / 2;
                sy += v.1 / 2;
            } else {
                half::<8>(d, o.ys, &r.luma(), (qx, qy), (v.0, v.1), self.rounding);
                sx += v.0;
                sy += v.1;
            }
        }
        self.chroma(o, r, x, y, chroma_four(sx), chroma_four(sy));
    }
}

/// Average `s` (rows `ss` apart) into the `W`×`W` block of `d`.
#[inline]
pub(crate) fn average<const W: usize>(d: &mut [u8], ds: usize, s: &[u8], ss: usize) {
    for (o, a) in d.chunks_mut(ds).zip(s.chunks(ss)).take(W) {
        let (Some(o), Some(a)) = (o.first_chunk_mut::<W>(), a.first_chunk::<W>()) else { return };
        for i in 0..W {
            o[i] = ((o[i] as u16 + a[i] as u16 + 1) >> 1) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        let mut f = Frame::new(2, 2);
        for (i, v) in f.y.iter_mut().enumerate() {
            *v = (i % 32 * 3 + i / 32 * 5) as u8;
        }
        f
    }

    #[test]
    fn half_sample_rounding_control() {
        let f = frame();
        let s = |x: usize, y: usize| u16::from(f.y[y * 32 + x]);
        let mut d = [0u8; 64];
        half::<8>(&mut d, 8, &f.luma(), (1, 1), (1, 1), 0);
        assert_eq!(u16::from(d[0]), (s(1, 1) + s(2, 1) + s(1, 2) + s(2, 2) + 2) >> 2);
        half::<8>(&mut d, 8, &f.luma(), (1, 1), (1, 1), 1);
        assert_eq!(u16::from(d[0]), (s(1, 1) + s(2, 1) + s(1, 2) + s(2, 2) + 1) >> 2);
        half::<8>(&mut d, 8, &f.luma(), (1, 1), (1, 0), 1);
        assert_eq!(u16::from(d[9]), (s(2, 2) + s(3, 2)) >> 1);
    }

    #[test]
    fn vectors_outside_the_frame_read_its_edges() {
        let f = frame();
        let mut d = [0u8; 256];
        half::<16>(&mut d, 16, &f.luma(), (0, 0), (-4000, -4000), 0);
        assert_eq!(d, [f.y[0]; 256]);
        half::<16>(&mut d, 16, &f.luma(), (16, 16), (4001, 4001), 0);
        assert_eq!(d, [f.y[32 * 32 - 1]; 256]);
        quarter::<16>(&mut d, 16, &f.luma(), (0, 0), (-9000, 9000), 0);
        assert_eq!(d, [f.y[31 * 32]; 256]);
    }

    /// A flat area stays flat at every quarter position; integer vectors copy.
    #[test]
    fn quarter_positions() {
        let mut f = Frame::new(2, 2);
        f.y.fill(77);
        let mut d = [0u8; 64];
        for mx in 0..4 {
            for my in 0..4 {
                quarter::<8>(&mut d, 8, &f.luma(), (4, 4), (mx, my), 0);
                assert_eq!(d, [77; 64], "{mx} {my}");
            }
        }
        let f = frame();
        quarter::<8>(&mut d, 8, &f.luma(), (4, 4), (4, 8), 0);
        assert_eq!(d[0], f.y[6 * 32 + 5]);
        assert_eq!(d[63], f.y[13 * 32 + 12]);
    }

    /// Half positions of the 8-tap filter, with the mirrored samples at the block's ends.
    #[test]
    fn eight_tap_filter_mirrors_at_the_block_ends() {
        let mut f = Frame::new(2, 2);
        for r in f.y.chunks_mut(32) {
            for (i, v) in r.iter_mut().enumerate() {
                *v = (i * 7 % 200) as u8;
            }
        }
        let mut d = [0u8; 64];
        quarter::<8>(&mut d, 8, &f.luma(), (0, 0), (2, 0), 0);
        let s = |i: usize| i32::from(f.y[i]);
        let first = (s(0) + s(1)) * 20 - (s(0) + s(2)) * 6 + (s(1) + s(3)) * 3 - (s(2) + s(4));
        assert_eq!(i32::from(d[0]), ((first + 16) >> 5).clamp(0, 255));
        let last = (s(7) + s(8)) * 20 - (s(6) + s(8)) * 6 + (s(5) + s(7)) * 3 - (s(4) + s(6));
        assert_eq!(i32::from(d[7]), ((last + 16) >> 5).clamp(0, 255));
    }

    #[test]
    fn chroma_vectors_follow_the_rounding_tables() {
        assert_eq!([0, 1, 2, 3, 4, -1, -2, -3, -4].map(|v| chroma_one(v, false)), [0, 1, 1, 1, 2, -1, -1, -1, -2]);
        assert_eq!([0, 2, 4, 8, 14, 16, -2, -4, -8].map(chroma_four), [0, 0, 1, 1, 2, 2, 0, -1, -1]);
    }
}
