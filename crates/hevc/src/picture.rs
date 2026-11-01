//! Decoded picture storage shared between decoding jobs: CTB rows are published once final (after
//! deblocking and SAO) together with the compressed motion field used for temporal motion vector
//! prediction.

use crate::Plane;
use std::sync::{Arc, OnceLock};

/// Motion data of one 16x16 block of a decoded picture (8.5.3.2.9 uses the motion of the top-left
/// 4x4 block of each 16x16 block).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColMv {
    pub mv: [[i16; 2]; 2],
    /// POC of the referenced picture per list.
    pub ref_poc: [i32; 2],
    /// bit0/1: predFlagL0/L1, bit2/3: reference is long-term (L0/L1). 0 = intra / unavailable.
    pub flags: u8,
}

/// Storage type of decoded samples: `u8` for 8-bit pictures (half the memory and cache traffic
/// of 16-bit storage in prediction reads, filters and output), `u16` for higher bit depths.
/// Values are always within `0..1 << bit_depth`.
pub trait Sample: Copy + Default + PartialEq + std::fmt::Debug + Send + Sync + 'static {
    fn to_i32(self) -> i32;
    fn to_i16(self) -> i16;
    fn to_u16(self) -> u16;
    /// `v` (a sample value, already clipped to the bit depth) in the storage type.
    fn from_i32(v: i32) -> Self;
    /// `v` (a sample value, within the bit depth) in the storage type.
    fn from_u16(v: u16) -> Self;
    /// The rows of `f` if they are stored with this type.
    fn rows(f: &Frame) -> Option<&[OnceLock<FrameRow<Self>>]>;
    fn wrap_rows(rows: Box<[OnceLock<FrameRow<Self>>]>) -> Rows;
}

impl Sample for u8 {
    #[inline(always)]
    fn to_i32(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn to_i16(self) -> i16 {
        self as i16
    }
    #[inline(always)]
    fn to_u16(self) -> u16 {
        self as u16
    }
    #[inline(always)]
    fn from_i32(v: i32) -> Self {
        v as u8
    }
    #[inline(always)]
    fn from_u16(v: u16) -> Self {
        v as u8
    }
    #[inline(always)]
    fn rows(f: &Frame) -> Option<&[OnceLock<FrameRow<u8>>]> {
        match &f.rows {
            Rows::U8(r) => Some(r),
            Rows::U16(_) => None,
        }
    }
    fn wrap_rows(rows: Box<[OnceLock<FrameRow<u8>>]>) -> Rows {
        Rows::U8(rows.into())
    }
}

impl Sample for u16 {
    #[inline(always)]
    fn to_i32(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn to_i16(self) -> i16 {
        self as i16
    }
    #[inline(always)]
    fn to_u16(self) -> u16 {
        self
    }
    #[inline(always)]
    fn from_i32(v: i32) -> Self {
        v as u16
    }
    #[inline(always)]
    fn from_u16(v: u16) -> Self {
        v
    }
    #[inline(always)]
    fn rows(f: &Frame) -> Option<&[OnceLock<FrameRow<u16>>]> {
        match &f.rows {
            Rows::U16(r) => Some(r),
            Rows::U8(_) => None,
        }
    }
    fn wrap_rows(rows: Box<[OnceLock<FrameRow<u16>>]>) -> Rows {
        Rows::U16(rows.into())
    }
}

/// Whether pictures with these bit depths are stored as `u8` (both 8-bit), else as `u16`.
pub fn stores_u8(bit_depth: u32, bit_depth_c: u32) -> bool {
    bit_depth <= 8 && bit_depth_c <= 8
}

/// One published CTB row.
pub struct FrameRow<S> {
    /// Luma lines of the row (stride = picture width).
    pub y: Box<[S]>,
    pub cb: Box<[S]>,
    pub cr: Box<[S]>,
    /// Motion per 16x16 block: (width16 * rows16) entries, raster within the row.
    pub col: Box<[ColMv]>,
}

/// The CTB rows of a frame, in its storage type.
#[derive(Clone)]
pub enum Rows {
    U8(Arc<[OnceLock<FrameRow<u8>>]>),
    U16(Arc<[OnceLock<FrameRow<u16>>]>),
}

/// A decoded (or in-progress) picture as stored in the DPB and referenced by later pictures.
pub struct Frame {
    pub id: u32,
    pub poc: i32,
    pub width: usize,
    pub height: usize,
    pub cwidth: usize,
    pub cheight: usize,
    pub log2_ctb: u32,
    pub bit_depth: u32,
    pub bit_depth_c: u32,
    rows: Rows,
}

/// The value of a published row (waiting for it with the `threads` feature).
#[inline(always)]
fn wait<T>(cell: &OnceLock<T>) -> Option<&T> {
    #[cfg(feature = "threads")]
    {
        Some(cell.wait())
    }
    #[cfg(not(feature = "threads"))]
    {
        cell.get()
    }
}

impl Frame {
    pub fn new(id: u32, poc: i32, width: usize, height: usize, log2_ctb: u32, bit_depth: u32, bit_depth_c: u32) -> Self {
        let n = height.div_ceil(1 << log2_ctb);
        fn cells<S: Sample>(n: usize) -> Rows {
            S::wrap_rows((0..n).map(|_| OnceLock::new()).collect())
        }
        let rows = if stores_u8(bit_depth, bit_depth_c) { cells::<u8>(n) } else { cells::<u16>(n) };
        Frame { id, poc, width, height, cwidth: width / 2, cheight: height / 2, log2_ctb, bit_depth, bit_depth_c, rows }
    }

    pub fn num_rows(&self) -> usize {
        match &self.rows {
            Rows::U8(r) => r.len(),
            Rows::U16(r) => r.len(),
        }
    }

    /// Number of luma lines in CTB row `r`.
    pub fn row_lines(&self, r: usize) -> usize {
        let ctb = 1usize << self.log2_ctb;
        ctb.min(self.height.saturating_sub(r * ctb))
    }

    pub fn width16(&self) -> usize {
        self.width.div_ceil(16)
    }

    /// Publish CTB row `r` (ignored if already published, or not of the frame's sample type).
    pub fn publish<S: Sample>(&self, r: usize, row: FrameRow<S>) {
        if let Some(cell) = S::rows(self).and_then(|rows| rows.get(r)) {
            let _ = cell.set(row);
        }
    }

    pub fn is_published(&self, r: usize) -> bool {
        match &self.rows {
            Rows::U8(rows) => rows.get(r).is_some_and(|c| c.get().is_some()),
            Rows::U16(rows) => rows.get(r).is_some_and(|c| c.get().is_some()),
        }
    }

    pub fn is_complete(&self) -> bool {
        let n = self.num_rows();
        n == 0 || self.is_published(n - 1)
    }

    /// CTB row `r` in sample type `S`, waiting until it has been published (None: no such row,
    /// or the frame stores another sample type).
    #[inline]
    pub fn row<S: Sample>(&self, r: usize) -> Option<&FrameRow<S>> {
        wait(S::rows(self)?.get(r)?)
    }

    /// The motion of CTB row `r`, waiting until it has been published.
    #[inline]
    fn col_row(&self, r: usize) -> Option<&[ColMv]> {
        match &self.rows {
            Rows::U8(rows) => wait(rows.get(r)?).map(|row| &*row.col),
            Rows::U16(rows) => wait(rows.get(r)?).map(|row| &*row.col),
        }
    }

    pub fn wait_complete(&self) {
        for r in 0..self.num_rows() {
            let _ = self.col_row(r);
        }
    }

    /// Motion of the 16x16 block containing luma sample (x, y) (inside the picture).
    #[inline]
    pub fn col_mv(&self, x: usize, y: usize) -> ColMv {
        let r = y >> self.log2_ctb;
        let y16 = (y & ((1 << self.log2_ctb) - 1)) >> 4;
        self.col_row(r).and_then(|col| col.get(y16 * self.width16() + (x >> 4))).copied().unwrap_or_default()
    }

    /// Copy a w x h luma window at (x0, y0) with edge replication into `out` (stride `os`).
    pub fn luma_window<S: Sample>(&self, (x0, y0): (i32, i32), (w, h): (usize, usize), out: &mut [i16], os: usize) {
        let (width, height) = (self.width as i32, self.height as i32);
        let sh = self.log2_ctb;
        let mask = (1i32 << sh) - 1;
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, height - 1);
            let Some(row) = self.row::<S>((yy >> sh) as usize) else { return };
            let off = ((yy & mask) * width) as usize;
            copy_line(&row.y[off..off + width as usize], x0, &mut out[r * os..r * os + w]);
        }
    }

    /// Copy a w x h window of chroma component `c` (0 = Cb, 1 = Cr) with edge replication.
    pub fn chroma_window<S: Sample>(&self, c: usize, (x0, y0): (i32, i32), (w, h): (usize, usize), out: &mut [i16], os: usize) {
        let (cw, ch) = (self.cwidth as i32, self.cheight as i32);
        let sh = self.log2_ctb - 1;
        let mask = (1i32 << sh) - 1;
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, ch - 1);
            let Some(row) = self.row::<S>((yy >> sh) as usize) else { return };
            let plane = if c == 0 { &row.cb } else { &row.cr };
            let off = ((yy & mask) * cw) as usize;
            copy_line(&plane[off..off + cw as usize], x0, &mut out[r * os..r * os + w]);
        }
    }

    /// The w-sample runs at x0 of the h lines from y0 of plane `c` (0 = luma, 1 = Cb, 2 = Cr) into
    /// `rows` (h = `rows.len()`), lines outside the picture replicated from its top / bottom edge
    /// (8-228, 8-229): the reference window of a prediction block without copying it. False (and
    /// `rows` partly filled) when the runs are not inside the picture horizontally.
    pub fn window_rows<'f, S: Sample>(&'f self, c: usize, x0: i32, y0: i32, w: usize, rows: &mut [&'f [S]]) -> bool {
        let (pw, ph, sh) = if c == 0 { (self.width, self.height, self.log2_ctb) } else { (self.cwidth, self.cheight, self.log2_ctb - 1) };
        let Some(x) = usize::try_from(x0).ok().filter(|&x| x.saturating_add(w) <= pw) else { return false };
        let mask = (1usize << sh) - 1;
        let max_y = ph as i32 - 1;
        let mut cur: Option<(usize, &'f [S])> = None;
        for (r, slot) in rows.iter_mut().enumerate() {
            let yy = y0.saturating_add(r as i32).min(max_y).max(0) as usize;
            let plane = match cur {
                Some((i, p)) if i == yy >> sh => p,
                _ => {
                    let Some(row) = self.row::<S>(yy >> sh) else { return false };
                    let p: &'f [S] = match c {
                        0 => &row.y,
                        1 => &row.cb,
                        _ => &row.cr,
                    };
                    cur = Some((yy >> sh, p));
                    p
                }
            };
            let off = (yy & mask) * pw + x;
            let Some(run) = plane.get(off..off + w) else { return false };
            *slot = run;
        }
        true
    }

    /// Copy the w x h block at (x0, y0) of plane `c` (0 = luma, 1 = Cb, 2 = Cr) into `dst`
    /// (stride `ds`), samples outside the picture replicated from its edges (8-228, 8-229).
    pub fn copy_block<S: Sample>(&self, c: usize, (x0, y0): (i32, i32), (w, h): (usize, usize), dst: &mut [S], ds: usize) {
        let (pw, ph, sh) = if c == 0 { (self.width, self.height, self.log2_ctb) } else { (self.cwidth, self.cheight, self.log2_ctb - 1) };
        let inside = usize::try_from(x0).ok().filter(|&x| x.saturating_add(w) <= pw);
        for (r, d) in dst.chunks_mut(ds).take(h).enumerate() {
            let yy = y0.saturating_add(r as i32).min(ph as i32 - 1).max(0) as usize;
            let Some(row) = self.row::<S>(yy >> sh) else { return };
            let plane = match c {
                0 => &row.y,
                1 => &row.cb,
                _ => &row.cr,
            };
            let off = (yy & ((1 << sh) - 1)) * pw;
            let Some(line) = plane.get(off..off + pw) else { return };
            let Some(d) = d.get_mut(..w) else { return };
            match inside.and_then(|x| line.get(x..x + w)) {
                Some(s) => copy_run(s, d),
                None => {
                    let max = (pw as i32 - 1).max(0);
                    for (i, d) in d.iter_mut().enumerate() {
                        *d = line.get(x0.saturating_add(i as i32).clamp(0, max) as usize).copied().unwrap_or_default();
                    }
                }
            }
        }
    }

    /// Crop the frame into planar buffers (waits for completion): 8-bit planes when `eight_bit`,
    /// else 16-bit ones. Rows stored in the output type are copied as they are.
    pub fn cropped_planes(&self, crop: (usize, usize, usize, usize), eight_bit: bool) -> (Plane, Plane, Plane) {
        fn planes<S: Sample, T>(f: &Frame, crop: (usize, usize, usize, usize), conv: impl Fn(S) -> T, wrap: impl Fn(Vec<T>) -> Plane) -> (Plane, Plane, Plane) {
            let (y, u, v) = f.copy_cropped::<S, T>(crop, conv);
            (wrap(y), wrap(u), wrap(v))
        }
        match (&self.rows, eight_bit) {
            (Rows::U8(_), true) => planes::<u8, u8>(self, crop, |v| v, Plane::U8),
            (Rows::U8(_), false) => planes::<u8, u16>(self, crop, u16::from, Plane::U16),
            // (8-bit luma with deeper chroma: the 8-bit output keeps the low bits, as before)
            (Rows::U16(_), true) => planes::<u16, u8>(self, crop, |v| v as u8, Plane::U8),
            (Rows::U16(_), false) => planes::<u16, u16>(self, crop, |v| v, Plane::U16),
        }
    }

    /// The cropped planes of the frame converted sample by sample with `conv` (a plain copy for
    /// the identity).
    fn copy_cropped<S: Sample, T>(&self, crop: (usize, usize, usize, usize), conv: impl Fn(S) -> T) -> (Vec<T>, Vec<T>, Vec<T>) {
        let (cx, cy, cw, ch) = crop;
        let ctb = 1usize << self.log2_ctb;
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        let put = |o: &mut Vec<T>, plane: &[S], off: usize, n: usize| {
            if let Some(s) = plane.get(off..off.saturating_add(n)) {
                o.extend(s.iter().map(|&v| conv(v)));
            }
        };
        y.reserve_exact(cw.saturating_mul(ch));
        for r in cy..cy.saturating_add(ch) {
            let Some(row) = self.row::<S>(r / ctb) else { break };
            put(&mut y, &row.y, (r % ctb) * self.width + cx, cw);
        }
        let (ccx, ccy, ccw, cch) = (cx / 2, cy / 2, cw.div_ceil(2), ch.div_ceil(2));
        let cctb = ctb / 2;
        u.reserve_exact(ccw.saturating_mul(cch));
        v.reserve_exact(ccw.saturating_mul(cch));
        for r in ccy..ccy.saturating_add(cch) {
            let Some(row) = self.row::<S>(r / cctb) else { break };
            let o = (r % cctb) * self.cwidth + ccx;
            put(&mut u, &row.cb, o, ccw);
            put(&mut v, &row.cr, o, ccw);
        }
        (y, u, v)
    }

    /// Publish every row not published yet as mid-grey without motion (a missing reference picture,
    /// or a picture whose decoding failed: readers never block on it).
    pub fn publish_gray(&self) {
        fn fill<S: Sample>(f: &Frame) {
            let (gy, gc) = (S::from_u16(1 << (f.bit_depth.clamp(1, 16) - 1)), S::from_u16(1 << (f.bit_depth_c.clamp(1, 16) - 1)));
            let w16 = f.width16();
            for r in 0..f.num_rows() {
                if f.is_published(r) {
                    continue;
                }
                let lines = f.row_lines(r);
                let clines = lines.div_ceil(2);
                f.publish(
                    r,
                    FrameRow::<S> {
                        y: vec![gy; lines * f.width].into(),
                        cb: vec![gc; clines * f.cwidth].into(),
                        cr: vec![gc; clines * f.cwidth].into(),
                        col: vec![ColMv::default(); w16 * lines.div_ceil(16)].into(),
                    },
                );
            }
        }
        match &self.rows {
            Rows::U8(_) => fill::<u8>(self),
            Rows::U16(_) => fill::<u16>(self),
        }
    }

    /// A mid-grey frame with the geometry of `like`.
    pub fn gray(id: u32, poc: i32, like: &Frame) -> Frame {
        let f = Frame::new(id, poc, like.width, like.height, like.log2_ctb, like.bit_depth, like.bit_depth_c);
        f.publish_gray();
        f
    }

    /// A distinct logical frame sharing this frame's rows. Used for fully published, immutable
    /// gray references: deblocking needs a unique frame ID even when their samples are identical.
    pub fn with_identity(&self, id: u32, poc: i32) -> Frame {
        Frame {
            id,
            poc,
            width: self.width,
            height: self.height,
            cwidth: self.cwidth,
            cheight: self.cheight,
            log2_ctb: self.log2_ctb,
            bit_depth: self.bit_depth,
            bit_depth_c: self.bit_depth_c,
            rows: self.rows.clone(),
        }
    }

    /// A frame with every row published from full-picture planes and no motion.
    #[cfg(test)]
    pub fn from_planes(id: u32, poc: i32, like: &Frame, y: &[u16], cb: &[u16], cr: &[u16]) -> Frame {
        fn fill<S: Sample>(f: &Frame, y: &[u16], cb: &[u16], cr: &[u16]) {
            let ctb = 1usize << f.log2_ctb;
            let w16 = f.width16();
            let conv = |s: &[u16]| -> Box<[S]> { s.iter().map(|&v| S::from_u16(v)).collect() };
            for r in 0..f.num_rows() {
                let lines = f.row_lines(r);
                let (y0, y1) = (r * ctb * f.width, (r * ctb + lines) * f.width);
                let clines = lines.div_ceil(2);
                let (c0, c1) = (r * ctb / 2 * f.cwidth, (r * ctb / 2 + clines) * f.cwidth);
                f.publish(
                    r,
                    FrameRow::<S> {
                        y: conv(&y[y0..y1]),
                        cb: conv(&cb[c0..c1]),
                        cr: conv(&cr[c0..c1]),
                        col: vec![ColMv::default(); w16 * lines.div_ceil(16)].into(),
                    },
                );
            }
        }
        let f = Frame::new(id, poc, like.width, like.height, like.log2_ctb, like.bit_depth, like.bit_depth_c);
        match &f.rows {
            Rows::U8(_) => fill::<u8>(&f, y, cb, cr),
            Rows::U16(_) => fill::<u16>(&f, y, cb, cr),
        }
        f
    }
}

/// `dst.copy_from_slice(src)` for the run lengths of prediction blocks as fixed-size copies: a
/// libc `memcpy` call per short block line costs more than the copy.
#[inline(always)]
fn copy_run<S: Sample>(src: &[S], dst: &mut [S]) {
    #[inline(always)]
    fn fixed<S: Sample, const N: usize>(src: &[S], dst: &mut [S]) {
        if let (Some(s), Some(d)) = (src.first_chunk::<N>(), dst.first_chunk_mut::<N>()) {
            *d = *s;
        }
    }
    match dst.len() {
        2 => fixed::<S, 2>(src, dst),
        4 => fixed::<S, 4>(src, dst),
        6 => fixed::<S, 6>(src, dst),
        8 => fixed::<S, 8>(src, dst),
        12 => fixed::<S, 12>(src, dst),
        16 => fixed::<S, 16>(src, dst),
        24 => fixed::<S, 24>(src, dst),
        32 => fixed::<S, 32>(src, dst),
        48 => fixed::<S, 48>(src, dst),
        64 => fixed::<S, 64>(src, dst),
        n => {
            if let Some(s) = src.get(..n) {
                dst.copy_from_slice(s);
            }
        }
    }
}

/// Copy `out.len()` samples of `line` starting at x0, replicating edge samples outside the line.
#[inline(always)]
fn copy_line<S: Sample>(line: &[S], x0: i32, out: &mut [i16]) {
    let w = out.len();
    if x0 >= 0 && x0 as usize + w <= line.len() {
        let src = &line[x0 as usize..x0 as usize + w];
        // fixed-size copies for the window widths (block width + 7 luma, + 3 chroma): see copy_run
        #[inline(always)]
        fn fixed<S: Sample, const N: usize>(src: &[S], out: &mut [i16]) {
            if let (Some(s), Some(o)) = (src.first_chunk::<N>(), out.first_chunk_mut::<N>()) {
                for j in 0..N {
                    o[j] = s[j].to_i16();
                }
            }
        }
        match w {
            5 => fixed::<S, 5>(src, out),
            7 => fixed::<S, 7>(src, out),
            9 => fixed::<S, 9>(src, out),
            11 => fixed::<S, 11>(src, out),
            15 => fixed::<S, 15>(src, out),
            19 => fixed::<S, 19>(src, out),
            23 => fixed::<S, 23>(src, out),
            27 => fixed::<S, 27>(src, out),
            31 => fixed::<S, 31>(src, out),
            35 => fixed::<S, 35>(src, out),
            39 => fixed::<S, 39>(src, out),
            55 => fixed::<S, 55>(src, out),
            71 => fixed::<S, 71>(src, out),
            _ => {
                for (o, &s) in out.iter_mut().zip(src) {
                    *o = s.to_i16();
                }
            }
        }
    } else {
        // the window crosses the left and / or right picture edge: the edge samples repeated
        // outside, the samples inside copied as a run
        let (Some(&first), Some(&last)) = (line.first(), line.last()) else { return };
        let n = line.len() as i64;
        let (x0, w) = (x0 as i64, w as i64);
        // out[i] is inside the line for i in lo..hi
        let lo = (-x0).clamp(0, w) as usize;
        let hi = (n - x0).clamp(0, w) as usize;
        let (left, rest) = out.split_at_mut(lo);
        left.fill(first.to_i16());
        let (mid, right) = rest.split_at_mut(hi.saturating_sub(lo));
        let start = (x0 + lo as i64).max(0) as usize;
        if let Some(src) = line.get(start..start + mid.len()) {
            for (o, &s) in mid.iter_mut().zip(src) {
                *o = s.to_i16();
            }
        }
        right.fill(last.to_i16());
    }
}

pub type FrameRef = Arc<Frame>;

/// An entry of RefPicList0/1 of the current slice.
pub type RefPic = crate::dpb::Ref<FrameRef>;

#[cfg(test)]
mod tests {
    use super::*;

    fn planes(w: usize, h: usize, bd: u32, seed: u64) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
        let mut x = seed;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % (1 << bd)) as u16
        };
        let y = (0..w * h).map(|_| next()).collect();
        let cb = (0..w * h / 4).map(|_| next()).collect();
        let cr = (0..w * h / 4).map(|_| next()).collect();
        (y, cb, cr)
    }

    /// 8-bit pictures are stored as `u8`, deeper ones as `u16`, and both crop back to exactly the
    /// samples they were given.
    #[test]
    fn storage_type_follows_the_bit_depth_and_crops_exactly() {
        let (w, h) = (40usize, 36usize);
        let crop = (2usize, 4usize, 33usize, 27usize);
        for bd in [8u32, 10] {
            let (y, cb, cr) = planes(w, h, bd, 0x9e37_79b9_7f4a_7c15 + bd as u64);
            let like = Frame::new(0, 0, w, h, 4, bd, bd);
            let f = Frame::from_planes(1, 0, &like, &y, &cb, &cr);
            assert_eq!(matches!(f.rows, Rows::U8(_)), bd == 8);
            assert!(f.is_complete());
            let (py, pu, pv) = f.cropped_planes(crop, bd == 8);
            let (cx, cy, cw, ch) = crop;
            let want_y: Vec<u16> = (cy..cy + ch).flat_map(|r| y[r * w + cx..r * w + cx + cw].to_vec()).collect();
            let (ccw, cch) = (cw.div_ceil(2), ch.div_ceil(2));
            let want_c = |p: &[u16]| -> Vec<u16> { (cy / 2..cy / 2 + cch).flat_map(|r| p[r * w / 2 + cx / 2..r * w / 2 + cx / 2 + ccw].to_vec()).collect() };
            let got = |p: &Plane| -> Vec<u16> { (0..p.len()).map(|i| p.get(i)).collect() };
            assert_eq!(got(&py), want_y, "luma bd {bd}");
            assert_eq!(got(&pu), want_c(&cb), "cb bd {bd}");
            assert_eq!(got(&pv), want_c(&cr), "cr bd {bd}");
            assert_eq!(py.as_u8().is_some(), bd == 8);
        }
    }

    /// Window lines crossing the left and / or right picture edge repeat the edge samples, as the
    /// per-sample clamp of 8-228 does.
    #[test]
    fn edge_replicated_lines_match_the_clamped_reads() {
        let line: Vec<u8> = (0..23).map(|i| (i * 11 % 256) as u8).collect();
        for n in [1usize, 2, 23] {
            let line = &line[..n];
            for w in [1usize, 5, 7, 11, 15, 19, 23, 39, 71] {
                for x0 in -80i32..80 {
                    let mut out = vec![-1i16; w];
                    copy_line(line, x0, &mut out);
                    let want: Vec<i16> = (0..w as i32).map(|i| line[(x0 + i).clamp(0, n as i32 - 1) as usize] as i16).collect();
                    assert_eq!(out, want, "n {n} w {w} x0 {x0}");
                }
            }
        }
    }

    /// Reading or publishing a frame with the other sample type (never done by the decoder) gives
    /// nothing instead of a panic, and a grey frame of either type is complete.
    #[test]
    fn wrong_sample_type_is_harmless() {
        let f = Frame::new(0, 0, 32, 32, 4, 8, 8);
        f.publish(0, FrameRow::<u16> { y: vec![1; 16 * 32].into(), cb: vec![1; 8 * 16].into(), cr: vec![1; 8 * 16].into(), col: Box::new([]) });
        assert!(!f.is_published(0));
        f.publish_gray();
        assert!(f.is_complete());
        assert!(f.row::<u16>(0).is_none());
        assert_eq!(f.row::<u8>(1).map(|r| r.y[0]), Some(128));
        let mut win = [7i16; 64];
        f.luma_window::<u16>((-3, -3), (8, 8), &mut win, 8);
        assert!(win.iter().all(|&v| v == 7));
        let mut dst = [5u16; 16];
        f.copy_block::<u16>(0, (0, 0), (4, 4), &mut dst, 4);
        assert!(dst.iter().all(|&v| v == 5));
        let g = Frame::gray(1, 0, &Frame::new(0, 0, 16, 8, 4, 10, 10));
        assert!(g.is_complete());
        assert_eq!(g.row::<u16>(0).map(|r| r.cb[0]), Some(512));
        assert_eq!(g.col_mv(15, 7), ColMv::default());
    }
}
