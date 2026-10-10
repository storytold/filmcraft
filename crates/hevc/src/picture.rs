//! Decoded picture storage shared between decoding jobs: CTB rows are published once final (after
//! deblocking and SAO) together with the compressed motion field used for temporal motion vector
//! prediction.

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

/// One published CTB row.
pub struct FrameRow {
    /// Luma lines of the row (stride = picture width).
    pub y: Box<[u16]>,
    pub cb: Box<[u16]>,
    pub cr: Box<[u16]>,
    /// Motion per 16x16 block: (width16 * rows16) entries, raster within the row.
    pub col: Box<[ColMv]>,
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
    rows: Box<[OnceLock<FrameRow>]>,
}

impl Frame {
    pub fn new(id: u32, poc: i32, width: usize, height: usize, log2_ctb: u32, bit_depth: u32, bit_depth_c: u32) -> Self {
        let n = height.div_ceil(1 << log2_ctb);
        Frame {
            id,
            poc,
            width,
            height,
            cwidth: width / 2,
            cheight: height / 2,
            log2_ctb,
            bit_depth,
            bit_depth_c,
            rows: (0..n).map(|_| OnceLock::new()).collect(),
        }
    }

    pub fn num_rows(&self) -> usize {
        self.rows.len()
    }

    /// Number of luma lines in CTB row `r`.
    pub fn row_lines(&self, r: usize) -> usize {
        let ctb = 1usize << self.log2_ctb;
        ctb.min(self.height - r * ctb)
    }

    pub fn width16(&self) -> usize {
        self.width.div_ceil(16)
    }

    /// Publish CTB row `r` (ignored if already published).
    pub fn publish(&self, r: usize, row: FrameRow) {
        let _ = self.rows[r].set(row);
    }

    pub fn is_published(&self, r: usize) -> bool {
        self.rows[r].get().is_some()
    }

    pub fn is_complete(&self) -> bool {
        self.rows.last().is_none_or(|r| r.get().is_some())
    }

    /// CTB row `r`, waiting until it has been published.
    #[inline]
    pub fn row(&self, r: usize) -> &FrameRow {
        #[cfg(feature = "threads")]
        {
            self.rows[r].wait()
        }
        #[cfg(not(feature = "threads"))]
        {
            self.rows[r].get().expect("reference row decoded before use")
        }
    }

    pub fn wait_complete(&self) {
        for r in 0..self.rows.len() {
            self.row(r);
        }
    }

    /// Motion of the 16x16 block containing luma sample (x, y) (inside the picture).
    #[inline]
    pub fn col_mv(&self, x: usize, y: usize) -> ColMv {
        let r = y >> self.log2_ctb;
        let row = self.row(r);
        let y16 = (y & ((1 << self.log2_ctb) - 1)) >> 4;
        row.col[y16 * self.width16() + (x >> 4)]
    }

    /// Copy a w x h luma window at (x0, y0) with edge replication into `out` (stride `os`).
    pub fn luma_window(&self, x0: i32, y0: i32, w: usize, h: usize, out: &mut [i16], os: usize) {
        let (width, height) = (self.width as i32, self.height as i32);
        let sh = self.log2_ctb;
        let mask = (1i32 << sh) - 1;
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, height - 1);
            let row = self.row((yy >> sh) as usize);
            let off = ((yy & mask) * width) as usize;
            copy_line(&row.y[off..off + width as usize], x0, &mut out[r * os..r * os + w]);
        }
    }

    /// Copy a w x h window of chroma component `c` (0 = Cb, 1 = Cr) with edge replication.
    pub fn chroma_window(&self, c: usize, x0: i32, y0: i32, w: usize, h: usize, out: &mut [i16], os: usize) {
        let (cw, ch) = (self.cwidth as i32, self.cheight as i32);
        let sh = self.log2_ctb - 1;
        let mask = (1i32 << sh) - 1;
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, ch - 1);
            let row = self.row((yy >> sh) as usize);
            let plane = if c == 0 { &row.cb } else { &row.cr };
            let off = ((yy & mask) * cw) as usize;
            copy_line(&plane[off..off + cw as usize], x0, &mut out[r * os..r * os + w]);
        }
    }

    /// Crop and copy the frame into planar buffers (waits for completion).
    pub fn copy_cropped(&self, crop: (usize, usize, usize, usize)) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
        let (cx, cy, cw, ch) = crop;
        let ctb = 1usize << self.log2_ctb;
        let mut y = Vec::with_capacity(cw * ch);
        for r in cy..cy + ch {
            let row = self.row(r / ctb);
            let o = (r % ctb) * self.width + cx;
            y.extend_from_slice(&row.y[o..o + cw]);
        }
        let (ccx, ccy, ccw, cch) = (cx / 2, cy / 2, cw.div_ceil(2), ch.div_ceil(2));
        let cctb = ctb / 2;
        let mut u = Vec::with_capacity(ccw * cch);
        let mut v = Vec::with_capacity(ccw * cch);
        for r in ccy..ccy + cch {
            let row = self.row(r / cctb);
            let o = (r % cctb) * self.cwidth + ccx;
            u.extend_from_slice(&row.cb[o..o + ccw]);
            v.extend_from_slice(&row.cr[o..o + ccw]);
        }
        (y, u, v)
    }

    /// A frame with every row published from full-picture planes and no motion (used to replace missing
    /// reference pictures).
    pub fn from_planes(id: u32, poc: i32, like: &Frame, y: &[u16], cb: &[u16], cr: &[u16]) -> Frame {
        let f = Frame::new(id, poc, like.width, like.height, like.log2_ctb, like.bit_depth, like.bit_depth_c);
        let ctb = 1usize << f.log2_ctb;
        let w16 = f.width16();
        for r in 0..f.num_rows() {
            let lines = f.row_lines(r);
            let (y0, y1) = (r * ctb * f.width, (r * ctb + lines) * f.width);
            let clines = lines.div_ceil(2);
            let (c0, c1) = (r * ctb / 2 * f.cwidth, (r * ctb / 2 + clines) * f.cwidth);
            f.publish(
                r,
                FrameRow { y: y[y0..y1].into(), cb: cb[c0..c1].into(), cr: cr[c0..c1].into(), col: vec![ColMv::default(); w16 * lines.div_ceil(16)].into() },
            );
        }
        f
    }

    /// A mid-grey frame with the geometry of `like`.
    pub fn gray(id: u32, poc: i32, like: &Frame) -> Frame {
        let y = vec![1u16 << (like.bit_depth - 1); like.width * like.height];
        let c = vec![1u16 << (like.bit_depth_c - 1); like.cwidth * like.cheight];
        Frame::from_planes(id, poc, like, &y, &c, &c)
    }
}

/// Copy `out.len()` samples of `line` starting at x0, replicating edge samples outside the line.
#[inline(always)]
fn copy_line(line: &[u16], x0: i32, out: &mut [i16]) {
    let w = out.len();
    if x0 >= 0 && x0 as usize + w <= line.len() {
        for (o, &s) in out.iter_mut().zip(&line[x0 as usize..x0 as usize + w]) {
            *o = s as i16;
        }
    } else {
        let max = line.len() as i32 - 1;
        for (c, o) in out.iter_mut().enumerate() {
            *o = line[(x0 + c as i32).clamp(0, max) as usize] as i16;
        }
    }
}

pub type FrameRef = Arc<Frame>;

/// An entry of RefPicList0/1 of the current slice.
pub type RefPic = crate::dpb::Ref<FrameRef>;
