//! Frame sample buffers and per-4x4 mode info storage.
//!
//! Tiles decode in parallel into private buffers that cover only the tile's area: a [`Plane`]
//! or [`MiInfo`] may hold a region of the frame (origin `ox`, `oy`), addressed with frame
//! coordinates throughout.

/// Recycled plane allocations: every frame allocates (and every evicted reference frees)
/// megabytes of planes, and fresh large allocations cost page faults and kernel calls. Buffers
/// are zeroed or fully overwritten on reuse, so contents never leak between frames.
static PLANE_POOL: std::sync::Mutex<Vec<Vec<u16>>> = std::sync::Mutex::new(Vec::new());
const POOL_MIN_LEN: usize = 1 << 16;
/// Samples kept at most (96 M samples = 192 MB, a few 4K frames).
const POOL_MAX_SAMPLES: usize = 96 << 20;

/// A zeroed buffer of `len` samples (recycled memory for large ones).
fn zeroed(len: usize) -> Vec<u16> {
    if let Some(mut v) = recycled(len) {
        v.clear();
        v.resize(len, 0);
        return v;
    }
    vec![0; len]
}

fn recycled(len: usize) -> Option<Vec<u16>> {
    if len >= POOL_MIN_LEN {
        let mut pool = PLANE_POOL.lock().unwrap_or_else(|e| e.into_inner());
        return pool.iter().position(|v| v.capacity() >= len && v.capacity() <= len.saturating_mul(2)).map(|i| pool.swap_remove(i));
    }
    None
}

/// One plane of samples (u16 at every bit depth), or a rectangular region of one.
#[derive(Default)]
pub struct Plane {
    pub data: Vec<u16>,
    pub stride: usize,
    /// Allocated rows.
    pub rows: usize,
    /// Frame coordinates of `data[0]` (0 for whole planes).
    pub ox: usize,
    pub oy: usize,
}

impl Drop for Plane {
    fn drop(&mut self) {
        if self.data.capacity() >= POOL_MIN_LEN {
            let v = std::mem::take(&mut self.data);
            let mut pool = PLANE_POOL.lock().unwrap_or_else(|e| e.into_inner());
            let kept: usize = pool.iter().map(|b| b.capacity()).sum();
            if kept + v.capacity() <= POOL_MAX_SAMPLES {
                pool.push(v);
            }
        }
    }
}

impl Plane {
    /// Fully initialize a recycled allocation by copying; no preceding zero fill is needed.
    ///
    /// Note: Plane deliberately does not implement Clone so that allocations remain
    /// fallible (Result) rather than panicking on OOM. Use     ry_clone instead.
    pub fn try_clone(&self) -> crate::Result<Plane> {
        let mut data = recycled(self.data.len()).unwrap_or_default();
        data.clear();
        data.try_reserve_exact(self.data.len()).map_err(|_| crate::Error::AllocationFailed)?;
        data.extend_from_slice(&self.data);
        Ok(Plane { data, stride: self.stride, rows: self.rows, ox: self.ox, oy: self.oy })
    }
    pub fn new(width: usize, height: usize) -> Plane {
        Plane { data: zeroed(width * height), stride: width, rows: height, ox: 0, oy: 0 }
    }
    /// A `width` x `height` region with its top-left sample at frame position (ox, oy).
    pub fn region(ox: usize, oy: usize, width: usize, height: usize) -> Plane {
        Plane { data: zeroed(width * height), stride: width, rows: height, ox, oy }
    }
    #[inline(always)]
    pub fn at(&self, x: usize, y: usize) -> u16 {
        self.data[(y - self.oy) * self.stride + x - self.ox]
    }
    #[inline(always)]
    pub fn set(&mut self, x: usize, y: usize, v: u16) {
        self.data[(y - self.oy) * self.stride + x - self.ox] = v;
    }
    /// Row `y`; index 0 is column `ox` (whole planes: column 0).
    #[inline(always)]
    pub fn row(&self, y: usize) -> &[u16] {
        let o = (y - self.oy) * self.stride;
        &self.data[o..o + self.stride]
    }
    #[inline(always)]
    pub fn row_mut(&mut self, y: usize) -> &mut [u16] {
        let o = (y - self.oy) * self.stride;
        &mut self.data[o..o + self.stride]
    }
    /// Row `y` from column `x` (frame coordinates) to the end of the stored row.
    #[inline(always)]
    pub fn row_from(&self, y: usize, x: usize) -> &[u16] {
        let o = (y - self.oy) * self.stride;
        &self.data[o + x - self.ox..o + self.stride]
    }
    #[inline(always)]
    pub fn row_from_mut(&mut self, y: usize, x: usize) -> &mut [u16] {
        let o = (y - self.oy) * self.stride;
        &mut self.data[o + x - self.ox..o + self.stride]
    }
    /// Copy this region into the whole plane `dst` (same frame coordinates).
    pub fn copy_into(&self, dst: &mut Plane) {
        for y in 0..self.rows {
            let s = &self.data[y * self.stride..(y + 1) * self.stride];
            dst.row_from_mut(self.oy + y, self.ox)[..self.stride].copy_from_slice(s);
        }
    }

    /// Disjoint mutable bands of whole rows, for filtering in parallel: one per `[start, end)`
    /// range of rows (relative to `oy`; ascending and not overlapping, else the later band is
    /// cut to what is left). Ranges are clipped to the stored rows.
    pub fn row_bands(&mut self, ranges: &[(usize, usize)]) -> Vec<RowBand<'_>> {
        let (stride, oy) = (self.stride.max(1), self.oy);
        let mut out = Vec::with_capacity(ranges.len());
        let mut rest = self.data.as_mut_slice();
        // First row of `rest`.
        let mut y = 0usize;
        for &(start, end) in ranges {
            let skip = start.saturating_sub(y).saturating_mul(stride).min(rest.len());
            rest = std::mem::take(&mut rest).split_at_mut(skip).1;
            y = y.saturating_add(skip / stride);
            let len = end.saturating_sub(y).saturating_mul(stride).min(rest.len());
            let (band, tail) = std::mem::take(&mut rest).split_at_mut(len);
            out.push(RowBand { data: band, stride, y0: oy.saturating_add(y) });
            y = y.saturating_add(len / stride);
            rest = tail;
        }
        out
    }
}

/// Mutable whole rows [y0, y0 + data.len() / stride) of a plane, addressed with the plane's
/// coordinates (see [`Plane::row_bands`]).
pub struct RowBand<'a> {
    pub data: &'a mut [u16],
    pub stride: usize,
    pub y0: usize,
}

impl RowBand<'_> {
    #[inline(always)]
    pub fn set(&mut self, x: usize, y: usize, v: u16) {
        self.data[(y - self.y0) * self.stride + x] = v;
    }
    #[inline(always)]
    pub fn row_mut(&mut self, y: usize) -> &mut [u16] {
        let o = (y - self.y0) * self.stride;
        &mut self.data[o..o + self.stride]
    }
}

/// A frame's three planes, sized to the macroblock-aligned coded area plus margins.
#[derive(Default)]
pub struct FrameBuf {
    pub planes: [Plane; 3],
    pub num_planes: usize,
    pub subsampling_x: usize,
    pub subsampling_y: usize,
    pub bit_depth: u8,
    /// Frame dimensions (luma) the samples are valid for.
    pub width: usize,
    pub height: usize,
}

impl FrameBuf {
    /// Note: FrameBuf deliberately does not implement Clone so that allocations remain
    /// fallible (Result) rather than panicking on OOM. Use     ry_clone instead.
    pub fn try_clone(&self) -> crate::Result<FrameBuf> {
        Ok(FrameBuf {
            planes: [self.planes[0].try_clone()?, self.planes[1].try_clone()?, self.planes[2].try_clone()?],
            num_planes: self.num_planes,
            subsampling_x: self.subsampling_x,
            subsampling_y: self.subsampling_y,
            bit_depth: self.bit_depth,
            width: self.width,
            height: self.height,
        })
    }
    /// Allocate a frame for a coded area of `width` x `height` luma samples (rounded up to the
    /// 128x128 superblock grid plus 160 samples of slack for blocks extending past the edge).
    pub fn new(width: usize, height: usize, num_planes: usize, subsampling_x: usize, subsampling_y: usize, bit_depth: u8) -> FrameBuf {
        let aw = width.div_ceil(128) * 128 + 160;
        let ah = height.div_ceil(128) * 128 + 160;
        let mut planes: [Plane; 3] = Default::default();
        planes[0] = Plane::new(aw, ah);
        for p in planes.iter_mut().take(num_planes).skip(1) {
            *p = Plane::new(aw >> subsampling_x, ah >> subsampling_y);
        }
        FrameBuf { planes, num_planes, subsampling_x, subsampling_y, bit_depth, width, height }
    }

    /// The region of a frame of this geometry covering luma samples [x0, x1) x [y0, y1)
    /// (chroma scaled down); the bounds are multiples of 8, or the allocated size.
    pub fn region_like(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> FrameBuf {
        let mut planes: [Plane; 3] = Default::default();
        for (p, pl) in planes.iter_mut().enumerate().take(self.num_planes) {
            let (sx, sy) = if p == 0 { (0, 0) } else { (self.subsampling_x, self.subsampling_y) };
            let full = &self.planes[p];
            let (px0, py0) = (x0 >> sx, y0 >> sy);
            let px1 = (x1 >> sx).min(full.stride);
            let py1 = (y1 >> sy).min(full.rows);
            *pl = Plane::region(px0, py0, px1 - px0, py1 - py0);
        }
        FrameBuf {
            planes,
            num_planes: self.num_planes,
            subsampling_x: self.subsampling_x,
            subsampling_y: self.subsampling_y,
            bit_depth: self.bit_depth,
            width: self.width,
            height: self.height,
        }
    }

    /// [`Plane::row_bands`] of all three planes for ranges of luma rows (chroma rows scaled
    /// down): element `i` holds the bands of range `i`.
    pub fn row_bands(&mut self, luma: &[(usize, usize)]) -> Vec<[RowBand<'_>; 3]> {
        let ssy = self.subsampling_y;
        let chroma: Vec<(usize, usize)> = luma.iter().map(|&(s, e)| (s >> ssy, e >> ssy)).collect();
        let [p0, p1, p2] = &mut self.planes;
        let (b1, b2) = (p1.row_bands(&chroma), p2.row_bands(&chroma));
        p0.row_bands(luma).into_iter().zip(b1).zip(b2).map(|((y, u), v)| [y, u, v]).collect()
    }

    pub fn plane_width(&self, plane: usize) -> usize {
        if plane == 0 { self.width } else { (self.width + self.subsampling_x) >> self.subsampling_x }
    }

    pub fn plane_height(&self, plane: usize) -> usize {
        if plane == 0 { self.height } else { (self.height + self.subsampling_y) >> self.subsampling_y }
    }
}

/// A motion vector (row, col) in 1/8 sample units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mv {
    pub row: i16,
    pub col: i16,
}

impl Mv {
    pub const ZERO: Mv = Mv { row: 0, col: 0 };
    pub fn new(row: i32, col: i32) -> Mv {
        Mv { row: row as i16, col: col as i16 }
    }
}

/// Mode info stored for every 4x4 luma position (the arrays the spec indexes by
/// `[ row ][ col ]` in mode-info units).
#[derive(Clone, Default)]
pub struct MiInfo {
    /// Frame size in mode info units.
    pub cols: usize,
    pub rows: usize,
    /// Stored region: origin and width (the whole frame unless a tile's private copy).
    pub ox: usize,
    pub oy: usize,
    pub stride: usize,
    pub region_rows: usize,
    pub y_mode: Vec<u8>,
    pub uv_mode: Vec<u8>,
    pub ref_frame: Vec<[i8; 2]>,
    pub mv: Vec<[Mv; 2]>,
    pub is_inter: Vec<bool>,
    pub skip_mode: Vec<bool>,
    pub skip: Vec<bool>,
    pub tx_size: Vec<u8>,
    pub inter_tx_size: Vec<u8>,
    pub mi_size: Vec<u8>,
    pub segment_id: Vec<u8>,
    pub palette_size: [Vec<u8>; 2],
    pub palette_colors: [Vec<[u16; 8]>; 2],
    pub delta_lf: Vec<[i8; 4]>,
    pub comp_group_idx: Vec<u8>,
    pub compound_idx: Vec<u8>,
    pub interp_filter: Vec<[u8; 2]>,
    pub motion_mode: Vec<u8>,
    /// TxTypes[ row ][ col ] (luma 4x4 units).
    pub tx_type: Vec<u8>,
    /// RefFrames[ row ][ col ] has been written for this frame.
    pub written: Vec<bool>,
}

impl MiInfo {
    /// `palette`: whether palette mode can occur (allow_screen_content_tools); without it the
    /// palette arrays (40 bytes per unit) stay empty.
    pub fn new(cols: usize, rows: usize, palette: bool) -> MiInfo {
        MiInfo::region(cols, rows, 0, 0, cols, rows, palette)
    }

    /// Storage for the w x h mode info units at (ox, oy) of a cols x rows frame.
    pub fn region(cols: usize, rows: usize, ox: usize, oy: usize, w: usize, h: usize, palette: bool) -> MiInfo {
        let n = w * h;
        let np = if palette { n } else { 0 };
        MiInfo {
            cols,
            rows,
            ox,
            oy,
            stride: w,
            region_rows: h,
            y_mode: vec![0; n],
            uv_mode: vec![0; n],
            ref_frame: vec![[0, -1]; n],
            mv: vec![[Mv::ZERO; 2]; n],
            is_inter: vec![false; n],
            skip_mode: vec![false; n],
            skip: vec![false; n],
            tx_size: vec![0; n],
            inter_tx_size: vec![0; n],
            mi_size: vec![0; n],
            segment_id: vec![0; n],
            palette_size: [vec![0; np], vec![0; np]],
            palette_colors: [vec![[0; 8]; np], vec![[0; 8]; np]],
            delta_lf: vec![[0; 4]; n],
            comp_group_idx: vec![0; n],
            compound_idx: vec![0; n],
            interp_filter: vec![[0; 2]; n],
            motion_mode: vec![0; n],
            tx_type: vec![0; n],
            written: vec![false; n],
        }
    }

    #[inline(always)]
    pub fn idx(&self, row: usize, col: usize) -> usize {
        debug_assert!(col >= self.ox && col < self.ox + self.stride && row >= self.oy, "mode info ({row}, {col}) outside the stored region");
        (row - self.oy) * self.stride + col - self.ox
    }

    /// Copy this region into the whole-frame `dst`.
    pub fn copy_into(&self, dst: &mut MiInfo) {
        let (w, h) = (self.stride, self.region_rows);
        macro_rules! copy {
            ($($f:ident)*) => {$(
                for y in 0..h {
                    let d = dst.idx(self.oy + y, self.ox);
                    dst.$f[d..d + w].copy_from_slice(&self.$f[y * w..(y + 1) * w]);
                }
            )*};
        }
        copy!(y_mode uv_mode ref_frame mv is_inter skip_mode skip tx_size inter_tx_size mi_size segment_id delta_lf comp_group_idx compound_idx interp_filter motion_mode tx_type written);
        for k in (0..2).filter(|&k| !self.palette_size[k].is_empty()) {
            for y in 0..h {
                let d = dst.idx(self.oy + y, self.ox);
                dst.palette_size[k][d..d + w].copy_from_slice(&self.palette_size[k][y * w..(y + 1) * w]);
                dst.palette_colors[k][d..d + w].copy_from_slice(&self.palette_colors[k][y * w..(y + 1) * w]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copying_planes_overwrites_recycled_samples_and_preserves_layout() {
        let len = POOL_MIN_LEN;
        let stale = Plane { data: vec![u16::MAX; len], stride: 256, rows: 256, ox: 0, oy: 0 };
        drop(stale);
        let source = Plane { data: (0..len).map(|i| (i % 1024) as u16).collect(), stride: 256, rows: 256, ox: 16, oy: 8 };
        let copied = source.try_clone().unwrap();
        assert_eq!(copied.data, source.data);
        assert_eq!((copied.stride, copied.rows, copied.ox, copied.oy), (256, 256, 16, 8));
        assert_ne!(copied.data.as_ptr(), source.data.as_ptr());
        assert!(Plane::default().try_clone().unwrap().data.is_empty());
    }

    #[test]
    fn copying_frames_preserves_padding_and_all_planes() {
        let mut source = FrameBuf::new(203, 117, 3, 1, 1, 10);
        for (plane, samples) in source.planes.iter_mut().enumerate() {
            samples.data.fill((plane as u16 + 1) * 127);
        }
        let copied = source.try_clone().unwrap();
        assert_eq!((copied.width, copied.height, copied.bit_depth, copied.num_planes), (203, 117, 10, 3));
        assert_eq!((copied.subsampling_x, copied.subsampling_y), (1, 1));
        for (a, b) in source.planes.iter().zip(&copied.planes) {
            assert_eq!(a.data, b.data);
            assert_eq!((a.stride, a.rows), (b.stride, b.rows));
        }
    }

    fn rows(b: &RowBand) -> (usize, usize) {
        (b.y0, b.data.len() / b.stride)
    }

    #[test]
    fn row_bands_split_whole_rows() {
        let mut p = Plane::new(8, 20);
        for (i, v) in p.data.iter_mut().enumerate() {
            *v = (i / 8) as u16;
        }
        let bands = p.row_bands(&[(0, 8), (8, 16), (16, usize::MAX)]);
        assert_eq!(bands.iter().map(rows).collect::<Vec<_>>(), [(0, 8), (8, 8), (16, 4)]);
        // Each band's row y holds the samples of plane row y.
        for mut b in bands {
            let y = b.y0 + 1;
            assert!(b.row_mut(y).iter().all(|&v| v as usize == y));
        }
        // Gaps between bands (the loop filter's even / odd turns).
        let bands = p.row_bands(&[(0, 4), (10, 14), (18, 30)]);
        assert_eq!(bands.iter().map(rows).collect::<Vec<_>>(), [(0, 4), (10, 4), (18, 2)]);
    }

    #[test]
    fn row_bands_never_overlap_or_panic() {
        let mut p = Plane::new(4, 10);
        // Overlapping, descending and out-of-range ranges are cut to what is left.
        let bands = p.row_bands(&[(2, 6), (4, 8), (1, 3), (9, 2), (50, 60), (usize::MAX, usize::MAX)]);
        assert_eq!(bands.iter().map(rows).collect::<Vec<_>>(), [(2, 4), (6, 2), (8, 0), (9, 0), (10, 0), (10, 0)]);
        let mut empty = Plane::default();
        assert!(empty.row_bands(&[(0, 64), (64, usize::MAX)]).iter().all(|b| b.data.is_empty()));
    }
}
