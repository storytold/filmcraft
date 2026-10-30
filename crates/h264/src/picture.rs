//! Picture buffers and per-macroblock state.

use std::sync::{Arc, OnceLock};

/// Planar 4:2:0 8-bit sample buffers (MB-aligned dimensions, stride == width).
#[derive(Clone)]
pub struct Planes {
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub cwidth: usize,
}

impl Planes {
    pub fn new(width: usize, height: usize) -> Self {
        let (cw, ch) = (width / 2, height / 2);
        Planes { y: vec![0; width * height], cb: vec![128; cw * ch], cr: vec![128; cw * ch], width, height, cwidth: cw }
    }
    pub fn gray(width: usize, height: usize) -> Self {
        let mut p = Self::new(width, height);
        p.y.fill(128);
        p
    }
}

/// Macroblock coding category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MbKind {
    #[default]
    None,
    I4x4,
    I8x8,
    I16x16,
    IPcm,
    PSkip,
    BSkip,
    BDirect16x16,
    /// Any other P or B inter macroblock.
    Inter,
}

impl MbKind {
    #[inline]
    pub fn is_intra(self) -> bool {
        matches!(self, MbKind::I4x4 | MbKind::I8x8 | MbKind::I16x16 | MbKind::IPcm)
    }
}

/// Per-macroblock state kept for the whole picture (neighbour derivations, deblocking, co-located data).
#[derive(Clone, Copy)]
pub struct MbState {
    /// Slice number within the picture; `u32::MAX` = not (yet) decoded.
    pub slice_num: u32,
    pub kind: MbKind,
    pub transform_8x8: bool,
    /// coded_block_pattern: bits 0..3 luma, bits 4..5 chroma.
    pub cbp: u8,
    pub qp: u8,
    /// QPc for Cb / Cr (used by deblocking).
    pub qpc: [u8; 2],
    pub intra_chroma_mode: u8,
    /// Intra4x4PredMode / Intra8x8PredMode per 4x4 block (raster order).
    pub intra_modes: [u8; 16],
    /// CAVLC: TotalCoeff per luma 4x4 block (raster). CABAC: coded_block_flag.
    pub nnz: [u8; 16],
    /// Same for chroma AC blocks [Cb, Cr][raster 2x2].
    pub nnz_c: [[u8; 4]; 2],
    /// CABAC coded_block_flag of DC blocks: bit0 luma (I16x16), bit1 Cb, bit2 Cr.
    pub cbf_dc: u8,
    /// Luma 4x4 blocks (raster bit) with non-zero coefficients, for deblocking bS = 2.
    pub nz_mask: u16,
    pub ref_idx: [[i8; 4]; 2],
    /// Motion vectors per 4x4 block (raster order).
    pub mv: [[[i16; 2]; 16]; 2],
    /// |mvd| per 4x4 block (raster), saturated, for CABAC context selection.
    pub mvd: [[[u8; 2]; 16]; 2],
    /// Bit per 8x8 block predicted in direct mode.
    pub direct8x8: u8,
}

impl MbState {
    #[inline]
    pub fn kind_is_skip(&self) -> bool {
        matches!(self.kind, MbKind::PSkip | MbKind::BSkip)
    }
}

impl Default for MbState {
    fn default() -> Self {
        MbState {
            slice_num: u32::MAX,
            kind: MbKind::None,
            transform_8x8: false,
            cbp: 0,
            qp: 0,
            qpc: [0; 2],
            intra_chroma_mode: 0,
            intra_modes: [2; 16],
            nnz: [0; 16],
            nnz_c: [[0; 4]; 2],
            cbf_dc: 0,
            nz_mask: 0,
            ref_idx: [[-1; 4]; 2],
            mv: [[[0; 2]; 16]; 2],
            mvd: [[[0; 2]; 16]; 2],
            direct8x8: 0,
        }
    }
}

/// One published macroblock row of a decoded frame: final (deblocked) samples plus the motion data
/// needed as co-located information for direct prediction.
pub struct FrameRow {
    /// 16 luma lines.
    pub y: Box<[u8]>,
    /// 8 lines per chroma component.
    pub cb: Box<[u8]>,
    pub cr: Box<[u8]>,
    /// Per 4x4 block: index mb_x * 16 + raster.
    pub mv: [Box<[[i16; 2]]>; 2],
    /// Per 8x8 block: index mb_x * 4 + b8.
    pub ref_idx: [Box<[i8]>; 2],
    /// Unique id of the referenced picture per 8x8 block (u32::MAX = none).
    pub ref_id: [Box<[u32]>; 2],
    pub intra: Box<[bool]>,
}

/// A decoded (or in-progress) frame as stored in the DPB and referenced by later pictures.
///
/// Rows are published in order by the thread decoding the picture; readers block (per row) until the
/// data they need is available, which lets several pictures decode concurrently.
pub struct Frame {
    pub id: u32,
    pub poc: i32,
    pub width: usize,
    pub height: usize,
    pub cwidth: usize,
    pub cheight: usize,
    pub mb_w: usize,
    rows: Box<[OnceLock<FrameRow>]>,
}

impl Frame {
    pub fn new(id: u32, poc: i32, mb_w: usize, mb_h: usize) -> Self {
        Frame { id, poc, width: mb_w * 16, height: mb_h * 16, cwidth: mb_w * 8, cheight: mb_h * 8, mb_w, rows: (0..mb_h).map(|_| OnceLock::new()).collect() }
    }

    pub fn mb_h(&self) -> usize {
        self.rows.len()
    }

    /// Publish MB row `r` (ignored if already published).
    pub fn publish(&self, r: usize, row: FrameRow) {
        let _ = self.rows[r].set(row);
    }

    pub fn is_published(&self, r: usize) -> bool {
        self.rows[r].get().is_some()
    }

    /// MB row `r`, waiting until it has been published.
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

    /// Wait until the whole frame is available.
    pub fn wait_complete(&self) {
        for r in 0..self.rows.len() {
            self.row(r);
        }
    }

    /// Build a published row from a contiguous plane set (rows of MB row `r`) and motion data.
    pub fn make_row(planes: &Planes, r: usize, mbs: &[MbState], ref_ids: &dyn Fn(&MbState, usize, i8) -> u32) -> FrameRow {
        let w = planes.width;
        let cw = planes.cwidth;
        let mb_w = w / 16;
        let y = planes.y[r * 16 * w..(r + 1) * 16 * w].into();
        let cb = planes.cb[r * 8 * cw..(r + 1) * 8 * cw].into();
        let cr = planes.cr[r * 8 * cw..(r + 1) * 8 * cw].into();
        let row_mbs = &mbs[r * mb_w..(r + 1) * mb_w];
        let mut mv = [vec![[0i16; 2]; mb_w * 16], vec![[0i16; 2]; mb_w * 16]];
        let mut ref_idx = [vec![-1i8; mb_w * 4], vec![-1i8; mb_w * 4]];
        let mut ref_id = [vec![u32::MAX; mb_w * 4], vec![u32::MAX; mb_w * 4]];
        let mut intra = vec![true; mb_w];
        for (x, st) in row_mbs.iter().enumerate() {
            if st.slice_num == u32::MAX {
                continue;
            }
            intra[x] = st.kind.is_intra();
            for l in 0..2 {
                mv[l][x * 16..x * 16 + 16].copy_from_slice(&st.mv[l]);
                for b in 0..4 {
                    let ri = st.ref_idx[l][b];
                    ref_idx[l][x * 4 + b] = ri;
                    if ri >= 0 {
                        ref_id[l][x * 4 + b] = ref_ids(st, l, ri);
                    }
                }
            }
        }
        let [mv0, mv1] = mv;
        let [ri0, ri1] = ref_idx;
        let [id0, id1] = ref_id;
        FrameRow { y, cb, cr, mv: [mv0.into(), mv1.into()], ref_idx: [ri0.into(), ri1.into()], ref_id: [id0.into(), id1.into()], intra: intra.into() }
    }

    /// A frame with every row published from `planes` and no motion (intra), e.g. "non-existing"
    /// frames inferred for frame_num gaps.
    pub fn from_planes(id: u32, poc: i32, planes: &Planes) -> Self {
        let (mb_w, mb_h) = (planes.width / 16, planes.height / 16);
        let f = Frame::new(id, poc, mb_w, mb_h);
        let blank = vec![MbState::default(); mb_w * mb_h];
        for r in 0..mb_h {
            f.publish(r, Frame::make_row(planes, r, &blank, &|_, _, _| u32::MAX));
        }
        f
    }

    /// Copy the w x h luma window at (x0, y0) (clamped to the picture) into `out` (stride `os`).
    #[inline]
    pub fn luma_window(&self, (x0, y0): (i32, i32), w: usize, h: usize, out: &mut [u8], os: usize) {
        let (width, height) = (self.width, self.height);
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, height as i32 - 1) as usize;
            let row = self.row(yy >> 4);
            let line = &row.y[(yy & 15) * width..(yy & 15) * width + width];
            copy_line::<24>(line, x0, &mut out[r * os..], w);
        }
    }

    /// Copy the w x h window of chroma component `c` at (x0, y0) (clamped) into `out`.
    #[inline]
    pub fn chroma_window(&self, c: usize, (x0, y0): (i32, i32), w: usize, h: usize, out: &mut [u8], os: usize) {
        let (cw, ch) = (self.cwidth, self.cheight);
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, ch as i32 - 1) as usize;
            let row = self.row(yy >> 3);
            let plane = if c == 0 { &row.cb } else { &row.cr };
            let line = &plane[(yy & 7) * cw..(yy & 7) * cw + cw];
            copy_line::<16>(line, x0, &mut out[r * os..], w);
        }
    }

    /// Crop and copy the frame into planar buffers (waits for completion).
    pub fn copy_cropped(&self, crop: (usize, usize, usize, usize)) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (cx, cy, cw, ch) = crop;
        let alloc = plane_allocator();
        let mut y = alloc(cw * ch);
        for r in cy..cy + ch {
            let row = self.row(r >> 4);
            y.extend_from_slice(&row.y[(r & 15) * self.width + cx..(r & 15) * self.width + cx + cw]);
        }
        let (ccx, ccy, ccw, cch) = (cx / 2, cy / 2, cw.div_ceil(2), ch.div_ceil(2));
        let mut u = alloc(ccw * cch);
        let mut v = alloc(ccw * cch);
        for r in ccy..ccy + cch {
            let row = self.row(r >> 3);
            let o = (r & 7) * self.cwidth + ccx;
            u.extend_from_slice(&row.cb[o..o + ccw]);
            v.extend_from_slice(&row.cr[o..o + ccw]);
        }
        (y, u, v)
    }
}

static PLANE_ALLOCATOR: std::sync::OnceLock<fn(usize) -> Vec<u8>> = std::sync::OnceLock::new();

/// Where output planes come from: an empty buffer with room for the given number of samples.
/// The host sets it once to recycle the planes of pictures it is done with; the first call wins.
pub fn set_plane_allocator(alloc: fn(usize) -> Vec<u8>) {
    let _ = PLANE_ALLOCATOR.set(alloc);
}

fn plane_allocator() -> fn(usize) -> Vec<u8> {
    PLANE_ALLOCATOR.get().copied().unwrap_or(Vec::with_capacity)
}

/// Copy `w` samples of `line` starting at x0 into `out`; when possible a fixed-size block of `N`
/// bytes is copied instead (cheaper than a variable-length copy; `out` must then have room for `N`).
#[inline(always)]
fn copy_line<const N: usize>(line: &[u8], x0: i32, out: &mut [u8], w: usize) {
    if w <= N
        && x0 >= 0
        && let Some(src) = line.get(x0 as usize..).and_then(|l| l.first_chunk::<N>())
        && let Some(dst) = out.first_chunk_mut::<N>()
    {
        *dst = *src;
    } else {
        copy_clamped(line, x0, &mut out[..w]);
    }
}

/// Copy `out.len()` samples of `line` starting at x0, replicating edge samples outside the line.
#[inline(always)]
fn copy_clamped(line: &[u8], x0: i32, out: &mut [u8]) {
    let w = out.len();
    if x0 >= 0 && x0 as usize + w <= line.len() {
        out.copy_from_slice(&line[x0 as usize..x0 as usize + w]);
    } else {
        let max = line.len() as i32 - 1;
        for (c, o) in out.iter_mut().enumerate() {
            *o = line[(x0 + c as i32).clamp(0, max) as usize];
        }
    }
}

pub type FrameRef = Arc<Frame>;

/// An entry of RefPicList0/1 for the current slice.
#[derive(Clone)]
pub struct RefPic {
    pub frame: FrameRef,
    pub long_term: bool,
}

impl RefPic {
    pub fn poc(&self) -> i32 {
        self.frame.poc
    }
    pub fn id(&self) -> u32 {
        self.frame.id
    }
}
