//! Picture buffers and per-macroblock state.

use crate::deblock::EdgeParams;
use std::sync::{Arc, OnceLock};

/// Storage type of decoded samples: `u8` for 8-bit pictures (half the memory and cache traffic of
/// 16-bit storage in prediction reads, filters and output), `u16` for higher bit depths. Values are
/// always within `0..1 << bit_depth`.
pub trait Sample: Copy + Default + PartialEq + std::fmt::Debug + Send + Sync + 'static {
    fn to_i32(self) -> i32;
    /// `v` (a sample value, already clipped to the bit depth) in the storage type.
    fn from_i32(v: i32) -> Self;
    /// `v` clipped to `0..=max` (`max` is `(1 << bit_depth) - 1`; 255 for `u8`, where it is a constant).
    fn clip(v: i32, max: i32) -> Self;
    /// A published copy of `s`.
    fn plane(s: &[Self]) -> Plane;
    /// Deblocking filter of the vertical edge whose q0 column is at `x`, for `N` lines from row `y`.
    fn filter_vertical<const N: usize>(pix: &mut [Self], stride: usize, x: usize, y: usize, ep: &EdgeParams, chroma: bool, max: i16);
    /// Deblocking filter of the horizontal edge whose q0 row is `y`, for `N` columns from column `x`.
    fn filter_horizontal<const N: usize>(pix: &mut [Self], stride: usize, x: usize, y: usize, ep: &EdgeParams, chroma: bool, max: i16);
    /// The samples of a published plane if it is stored with this type.
    fn plane_slice(p: &Plane) -> Option<&[Self]>;
}

impl Sample for u8 {
    #[inline(always)]
    fn to_i32(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn from_i32(v: i32) -> Self {
        v as u8
    }
    #[inline(always)]
    fn clip(v: i32, _max: i32) -> Self {
        v.clamp(0, 255) as u8
    }
    fn plane(s: &[u8]) -> Plane {
        Plane::U8(s.into())
    }
    #[inline(always)]
    fn filter_vertical<const N: usize>(pix: &mut [u8], stride: usize, x: usize, y: usize, ep: &EdgeParams, chroma: bool, max: i16) {
        let _ = max;
        crate::deblock::filter_vertical_u8::<N>(pix, stride, x, y, ep, chroma, 255);
    }
    #[inline(always)]
    fn filter_horizontal<const N: usize>(pix: &mut [u8], stride: usize, x: usize, y: usize, ep: &EdgeParams, chroma: bool, max: i16) {
        let _ = max;
        crate::deblock::filter_horizontal_generic::<u8, N>(pix, stride, x, y, ep, chroma, 255);
    }
    #[inline(always)]
    fn plane_slice(p: &Plane) -> Option<&[u8]> {
        match p {
            Plane::U8(v) => Some(v),
            Plane::U16(_) => None,
        }
    }
}

impl Sample for u16 {
    #[inline(always)]
    fn to_i32(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn from_i32(v: i32) -> Self {
        v as u16
    }
    #[inline(always)]
    fn clip(v: i32, max: i32) -> Self {
        v.clamp(0, max) as u16
    }
    fn plane(s: &[u16]) -> Plane {
        Plane::U16(s.into())
    }
    #[inline(always)]
    fn filter_vertical<const N: usize>(pix: &mut [u16], stride: usize, x: usize, y: usize, ep: &EdgeParams, chroma: bool, max: i16) {
        crate::deblock::filter_vertical_generic::<u16, N>(pix, stride, x, y, ep, chroma, max);
    }
    #[inline(always)]
    fn filter_horizontal<const N: usize>(pix: &mut [u16], stride: usize, x: usize, y: usize, ep: &EdgeParams, chroma: bool, max: i16) {
        crate::deblock::filter_horizontal_generic::<u16, N>(pix, stride, x, y, ep, chroma, max);
    }
    #[inline(always)]
    fn plane_slice(p: &Plane) -> Option<&[u16]> {
        match p {
            Plane::U16(v) => Some(v),
            Plane::U8(_) => None,
        }
    }
}

/// Planar sample buffers (MB-aligned dimensions, stride == width): 4:2:0 or 4:2:2.
#[derive(Clone)]
pub struct Planes<S: Sample> {
    pub y: Vec<S>,
    pub cb: Vec<S>,
    pub cr: Vec<S>,
    pub width: usize,
    pub height: usize,
    pub cwidth: usize,
    pub cheight: usize,
}

impl<S: Sample> Planes<S> {
    /// Luma 0, chroma at the mid value `mid` (`1 << (bit_depth - 1)`).
    pub fn new(width: usize, height: usize, c422: bool, mid: i32) -> Self {
        let (cw, ch) = (width / 2, if c422 { height } else { height / 2 });
        Planes {
            y: vec![S::default(); width * height],
            cb: vec![S::from_i32(mid); cw * ch],
            cr: vec![S::from_i32(mid); cw * ch],
            width,
            height,
            cwidth: cw,
            cheight: ch,
        }
    }
    pub fn gray(width: usize, height: usize, c422: bool, mid: i32) -> Self {
        let mut p = Self::new(width, height, c422, mid);
        p.y.fill(S::from_i32(mid));
        p
    }
    /// Chroma lines per macroblock row.
    pub fn mb_hc(&self) -> usize {
        if self.height == 0 { 8 } else { self.cheight / (self.height / 16) }
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
    /// QPY (negative for high bit depths: -QpBdOffsetY..=51).
    pub qp: i8,
    /// QPc for Cb / Cr (used by deblocking).
    pub qpc: [i8; 2],
    pub intra_chroma_mode: u8,
    /// Intra4x4PredMode / Intra8x8PredMode per 4x4 block (raster order).
    pub intra_modes: [u8; 16],
    /// CAVLC: TotalCoeff per luma 4x4 block (raster). CABAC: coded_block_flag.
    pub nnz: [u8; 16],
    /// Same for chroma AC blocks [Cb, Cr][raster, 2 blocks wide: 2x2 for 4:2:0, 2x4 for 4:2:2].
    pub nnz_c: [[u8; 8]; 2],
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
            nnz_c: [[0; 8]; 2],
            cbf_dc: 0,
            nz_mask: 0,
            ref_idx: [[-1; 4]; 2],
            mv: [[[0; 2]; 16]; 2],
            mvd: [[[0; 2]; 16]; 2],
            direct8x8: 0,
        }
    }
}

/// One plane of a published macroblock row.
#[derive(Clone)]
pub enum Plane {
    U8(Box<[u8]>),
    U16(Box<[u16]>),
}

/// One published macroblock row of a decoded frame: final (deblocked) samples plus the motion data
/// needed as co-located information for direct prediction.
pub struct FrameRow {
    /// 16 luma lines.
    pub y: Plane,
    /// 8 (4:2:0) or 16 (4:2:2) lines per chroma component.
    pub cb: Plane,
    pub cr: Plane,
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
    /// Chroma lines per macroblock row: 8 (4:2:0) or 16 (4:2:2).
    pub mb_hc: usize,
    /// Bits per sample (8: rows are `Plane::U8`, more: `Plane::U16`).
    pub bit_depth: u32,
    rows: Box<[OnceLock<FrameRow>]>,
}

impl Frame {
    /// A frame with `mb_hc` chroma lines per macroblock row (8 or 16) and `bit_depth` bits per sample.
    pub fn with_format(id: u32, poc: i32, mb_w: usize, mb_h: usize, mb_hc: usize, bit_depth: u32) -> Self {
        Frame {
            id,
            poc,
            width: mb_w * 16,
            height: mb_h * 16,
            cwidth: mb_w * 8,
            cheight: mb_h * mb_hc,
            mb_w,
            mb_hc,
            bit_depth,
            rows: (0..mb_h).map(|_| OnceLock::new()).collect(),
        }
    }

    pub fn mb_h(&self) -> usize {
        self.rows.len()
    }

    /// Samples are stored in `u16`.
    pub fn is_wide(&self) -> bool {
        self.bit_depth > 8
    }

    /// Publish MB row `r` (ignored if already published).
    pub fn publish(&self, r: usize, row: FrameRow) {
        if let Some(slot) = self.rows.get(r) {
            let _ = slot.set(row);
        }
    }

    pub fn is_published(&self, r: usize) -> bool {
        self.rows.get(r).is_some_and(|s| s.get().is_some())
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
    pub fn make_row<S: Sample>(planes: &Planes<S>, r: usize, mbs: &[MbState], ref_ids: &dyn Fn(&MbState, usize, i8) -> u32) -> FrameRow {
        let w = planes.width;
        let cw = planes.cwidth;
        let hc = planes.mb_hc();
        let mb_w = w / 16;
        let y = S::plane(&planes.y[r * 16 * w..(r + 1) * 16 * w]);
        let cb = S::plane(&planes.cb[r * hc * cw..(r + 1) * hc * cw]);
        let cr = S::plane(&planes.cr[r * hc * cw..(r + 1) * hc * cw]);
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
    pub fn from_planes<S: Sample>(id: u32, poc: i32, planes: &Planes<S>, bit_depth: u32) -> Self {
        let (mb_w, mb_h) = (planes.width / 16, planes.height / 16);
        let f = Frame::with_format(id, poc, mb_w, mb_h, planes.mb_hc(), bit_depth);
        let blank = vec![MbState::default(); mb_w * mb_h];
        for r in 0..mb_h {
            f.publish(r, Frame::make_row(planes, r, &blank, &|_, _, _| u32::MAX));
        }
        f
    }

    /// Copy the w x h luma window at (x0, y0) (clamped to the picture) into `out` (stride `os`).
    /// A frame stored with the other sample type leaves `out` untouched.
    #[inline]
    pub fn luma_window<S: Sample>(&self, x0: i32, y0: i32, w: usize, h: usize, out: &mut [S], os: usize) {
        let (width, height) = (self.width, self.height);
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, height as i32 - 1) as usize;
            let row = self.row(yy >> 4);
            let Some(y) = S::plane_slice(&row.y) else { return };
            let line = &y[(yy & 15) * width..(yy & 15) * width + width];
            copy_line::<S, 24>(line, x0, &mut out[r * os..], w);
        }
    }

    /// Copy the w x h window of chroma component `c` at (x0, y0) (clamped) into `out`.
    #[inline]
    pub fn chroma_window<S: Sample>(&self, c: usize, x0: i32, y0: i32, w: usize, h: usize, out: &mut [S], os: usize) {
        let (cw, ch) = (self.cwidth, self.cheight);
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, ch as i32 - 1) as usize;
            let row = self.row(yy / self.mb_hc);
            let Some(plane) = S::plane_slice(if c == 0 { &row.cb } else { &row.cr }) else { return };
            let l = yy % self.mb_hc;
            let line = &plane[l * cw..l * cw + cw];
            copy_line::<S, 16>(line, x0, &mut out[r * os..], w);
        }
    }

    /// Crop and copy an 8-bit frame into planar buffers (waits for completion). `crop` is
    /// (x, y, width, height) in luma samples. A wide frame yields empty planes: use
    /// [`Frame::copy_cropped_wide`].
    pub fn copy_cropped(&self, crop: (usize, usize, usize, usize)) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (cx, cy, cw, ch) = crop;
        let alloc = plane_allocator();
        let mut y = alloc(cw * ch);
        let hc = self.mb_hc;
        let (ccx, ccy, ccw, cch) = self.chroma_crop(crop);
        let mut u = alloc(ccw * cch);
        let mut v = alloc(ccw * cch);
        for r in cy..cy + ch {
            let row = self.row(r >> 4);
            let Plane::U8(p) = &row.y else { return (y, u, v) };
            y.extend_from_slice(&p[(r & 15) * self.width + cx..(r & 15) * self.width + cx + cw]);
        }
        for r in ccy..ccy + cch {
            let row = self.row(r / hc);
            let (Plane::U8(cb), Plane::U8(cr)) = (&row.cb, &row.cr) else { return (y, u, v) };
            let o = (r % hc) * self.cwidth + ccx;
            u.extend_from_slice(&cb[o..o + ccw]);
            v.extend_from_slice(&cr[o..o + ccw]);
        }
        (y, u, v)
    }

    /// [`Frame::copy_cropped`] for a wide (more than 8 bits per sample) frame; an 8-bit frame
    /// yields empty planes.
    pub fn copy_cropped_wide(&self, crop: (usize, usize, usize, usize)) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
        let (cx, cy, cw, ch) = crop;
        let hc = self.mb_hc;
        let (ccx, ccy, ccw, cch) = self.chroma_crop(crop);
        let (mut y, mut u, mut v) = (Vec::with_capacity(cw * ch), Vec::with_capacity(ccw * cch), Vec::with_capacity(ccw * cch));
        for r in cy..cy + ch {
            let row = self.row(r >> 4);
            let Plane::U16(p) = &row.y else { return (y, u, v) };
            y.extend_from_slice(&p[(r & 15) * self.width + cx..(r & 15) * self.width + cx + cw]);
        }
        for r in ccy..ccy + cch {
            let row = self.row(r / hc);
            let (Plane::U16(cb), Plane::U16(cr)) = (&row.cb, &row.cr) else { return (y, u, v) };
            let o = (r % hc) * self.cwidth + ccx;
            u.extend_from_slice(&cb[o..o + ccw]);
            v.extend_from_slice(&cr[o..o + ccw]);
        }
        (y, u, v)
    }

    /// [`Frame::copy_cropped_wide`] with the lines of each plane copied by `pool`'s threads (a 4K
    /// 4:2:2 picture is 33 MB: copied on the calling thread it would be the serial part of
    /// decoding that limits frame-threaded speed).
    #[cfg(feature = "threads")]
    pub fn copy_cropped_wide_in(&self, crop: (usize, usize, usize, usize), pool: &rayon::ThreadPool) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
        use rayon::prelude::*;
        let (cx, cy, cw, ch) = crop;
        let hc = self.mb_hc;
        let (ccx, ccy, ccw, cch) = self.chroma_crop(crop);
        if cw == 0 || ccw == 0 {
            return Default::default();
        }
        // (zeroed allocations come from fresh pages: the page faults happen on the copying threads)
        let (mut y, mut u, mut v) = (vec![0u16; cw * ch], vec![0u16; ccw * cch], vec![0u16; ccw * cch]);
        pool.install(|| {
            y.par_chunks_mut(cw).enumerate().with_min_len(8).for_each(|(i, line)| {
                let r = cy + i;
                if let Plane::U16(p) = &self.row(r >> 4).y {
                    line.copy_from_slice(&p[(r & 15) * self.width + cx..(r & 15) * self.width + cx + cw]);
                }
            });
            for (plane, pick) in [(&mut u, 0), (&mut v, 1)] {
                plane.par_chunks_mut(ccw).enumerate().with_min_len(8).for_each(|(i, line)| {
                    let r = ccy + i;
                    let row = self.row(r / hc);
                    if let Plane::U16(p) = if pick == 0 { &row.cb } else { &row.cr } {
                        let o = (r % hc) * self.cwidth + ccx;
                        line.copy_from_slice(&p[o..o + ccw]);
                    }
                });
            }
        });
        (y, u, v)
    }

    /// The chroma crop rectangle (x, y, width, height) matching a luma crop.
    fn chroma_crop(&self, crop: (usize, usize, usize, usize)) -> (usize, usize, usize, usize) {
        let (cx, cy, cw, ch) = crop;
        if self.mb_hc == 16 { (cx / 2, cy, cw.div_ceil(2), ch) } else { (cx / 2, cy / 2, cw.div_ceil(2), ch.div_ceil(2)) }
    }
}

static PLANE_ALLOCATOR: std::sync::OnceLock<fn(usize) -> Vec<u8>> = std::sync::OnceLock::new();

/// Where output planes come from: an empty buffer with room for the given number of samples.
/// The host sets it once to recycle the planes of pictures it is done with; the first call wins.
/// (It serves 8-bit pictures only.)
pub fn set_plane_allocator(alloc: fn(usize) -> Vec<u8>) {
    let _ = PLANE_ALLOCATOR.set(alloc);
}

fn plane_allocator() -> fn(usize) -> Vec<u8> {
    PLANE_ALLOCATOR.get().copied().unwrap_or(Vec::with_capacity)
}

/// Copy `w` samples of `line` starting at x0 into `out`; when possible a fixed-size block of `N`
/// bytes is copied instead (cheaper than a variable-length copy; `out` must then have room for `N`).
#[inline(always)]
fn copy_line<S: Sample, const N: usize>(line: &[S], x0: i32, out: &mut [S], w: usize) {
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
fn copy_clamped<S: Sample>(line: &[S], x0: i32, out: &mut [S]) {
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
