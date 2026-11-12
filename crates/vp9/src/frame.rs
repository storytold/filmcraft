//! Decoded frame storage and the per-8x8 mode info grid.

use crate::tables::{INTRA_FRAME, NONE};

/// A motion vector in 1/8 sample units.
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

/// Mode info of one 8x8 block (the arrays of 6.4.4: Skips, TxSizes, MiSizes, YModes, SubModes,
/// RefFrames, InterpFilters, Mvs / SubMvs).
#[derive(Clone, Copy, Debug)]
pub struct MiInfo {
    pub sb_size: u8,
    pub skip: bool,
    pub tx_size: u8,
    pub y_mode: u8,
    pub sub_modes: [u8; 4],
    pub seg_id: u8,
    pub ref_frame: [i8; 2],
    pub interp_filter: u8,
    /// SubMvs[refList][b]; Mvs[refList] is `mv[refList][3]`.
    pub mv: [[Mv; 4]; 2],
}

impl Default for MiInfo {
    fn default() -> Self {
        MiInfo {
            sb_size: 0,
            skip: false,
            tx_size: 0,
            y_mode: 0,
            sub_modes: [0; 4],
            seg_id: 0,
            ref_frame: [INTRA_FRAME, NONE],
            interp_filter: 0,
            mv: [[Mv::ZERO; 4]; 2],
        }
    }
}

/// Mode info of a whole frame (MiRows x MiCols).
#[derive(Clone, Debug, Default)]
pub struct MiGrid {
    pub cols: usize,
    pub rows: usize,
    pub mi: Vec<MiInfo>,
}

impl MiGrid {
    #[inline]
    pub fn at(&self, r: usize, c: usize) -> &MiInfo {
        &self.mi[r * self.cols + c]
    }
}

/// Scratch rows above the samples of every band: the loop filter of superblock row r reads 8
/// and modifies up to 7 rows of row r - 1 (the 16-wide filter of the horizontal edges on top of
/// row r), so those rows are copied there while row r is filtered (see `decoder::post`).
pub const BAND_PAD: usize = 8;

/// One superblock row (64 luma rows) of a frame: per plane `BAND_PAD` scratch rows followed by
/// the band's rows, all `Frame::strides[p]` samples wide.
pub struct Band {
    pub planes: [Vec<u16>; 3],
}

/// Frame metadata from the frame header.
#[derive(Clone, Copy, Debug)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub ss_x: bool,
    pub ss_y: bool,
    pub bit_depth: u8,
    pub color_space: u8,
    pub color_range: bool,
    pub render_width: u32,
    pub render_height: u32,
    pub key: bool,
    pub intra_only: bool,
}

/// A decoded frame (also used as reference). Its samples are published one superblock row
/// ("band") at a time, once loop filtered, so that later frames can predict from the finished
/// top of a frame while its bottom is still being filtered (frame threading).
pub struct Frame {
    pub info: FrameInfo,
    /// Allocated plane widths (whole superblocks) = row strides.
    pub strides: [usize; 3],
    /// Rows per band of each plane.
    pub band_h: [usize; 3],
    /// Visible (cropped) plane sizes.
    pub vis: [(usize, usize); 3],
    bands: Box<[std::sync::OnceLock<Band>]>,
    pools: std::sync::Arc<Pools>,
}

impl Frame {
    pub fn new(info: FrameInfo, pools: std::sync::Arc<Pools>) -> Frame {
        let geo = plane_geometry(info.width, info.height, info.ss_x, info.ss_y);
        let sb_rows = (info.height as usize).div_ceil(64);
        Frame {
            info,
            strides: geo.map(|g| g.0),
            band_h: [64, 64 >> info.ss_y as usize, 64 >> info.ss_y as usize],
            vis: geo.map(|g| (g.2, g.3)),
            bands: (0..sb_rows).map(|_| std::sync::OnceLock::new()).collect(),
            pools,
        }
    }

    pub fn sb_rows(&self) -> usize {
        self.bands.len()
    }

    /// A band buffer for this frame's geometry (recycled memory, contents unspecified).
    pub fn new_band(&self) -> Band {
        Band { planes: std::array::from_fn(|p| self.pools.bands.take_any((self.band_h[p] + BAND_PAD) * self.strides[p], 0)) }
    }

    /// Publish band `r` (ignored if already published).
    pub fn publish(&self, r: usize, band: Band) {
        if let Err(b) = self.bands[r].set(band) {
            for p in b.planes {
                self.pools.bands.put(p);
            }
        }
    }

    pub fn is_published(&self, r: usize) -> bool {
        self.bands[r].get().is_some()
    }

    pub fn is_complete(&self) -> bool {
        self.bands.iter().all(|b| b.get().is_some())
    }

    /// Band `r`, waiting until it has been published.
    #[inline]
    pub fn band(&self, r: usize) -> &Band {
        #[cfg(feature = "threads")]
        {
            self.bands[r].wait()
        }
        #[cfg(not(feature = "threads"))]
        {
            // Bands are decoded in order before use; an unpublished one reads as empty.
            static EMPTY: Band = Band { planes: [Vec::new(), Vec::new(), Vec::new()] };
            self.bands[r].get().unwrap_or(&EMPTY)
        }
    }

    /// Row `y` of plane `p` (all `strides[p]` samples), waiting until it is final.
    #[inline]
    pub fn row(&self, p: usize, y: usize) -> &[u16] {
        let bh = self.band_h[p];
        let s = self.strides[p];
        let o = (y % bh + BAND_PAD) * s;
        &self.band(y / bh).planes[p][o..o + s]
    }

    /// Rows `y0..y1` of plane `p` as one slice with stride `strides[p]` when they lie in one band:
    /// (band samples, offset of row y0).
    #[inline]
    pub fn span(&self, p: usize, y0: usize, y1: usize) -> Option<(&[u16], usize)> {
        let bh = self.band_h[p];
        let r = y0 / bh;
        if (y1 - 1) / bh != r {
            return None;
        }
        Some((&self.band(r).planes[p], (y0 % bh + BAND_PAD) * self.strides[p]))
    }

    /// A complete frame from full planes (tests).
    #[cfg(test)]
    pub fn from_planes(info: FrameInfo, planes: [&[u16]; 3]) -> Frame {
        let f = Frame::new(info, Default::default());
        for r in 0..f.sb_rows() {
            let mut b = f.new_band();
            for p in 0..3 {
                let (s, bh) = (f.strides[p], f.band_h[p]);
                for k in 0..bh {
                    let y = r * bh + k;
                    if let Some(src) = planes[p].get(y * s..y * s + s) {
                        b.planes[p][(k + BAND_PAD) * s..(k + BAND_PAD + 1) * s].copy_from_slice(src);
                    }
                }
            }
            f.publish(r, b);
        }
        f
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        for b in std::mem::take(&mut self.bands).into_vec().into_iter().filter_map(|b| b.into_inner()) {
            for p in b.planes {
                self.pools.bands.put(p);
            }
        }
    }
}

/// Geometry of the frame planes: allocation sizes for (MiCols, MiRows) rounded to superblocks.
pub fn plane_geometry(width: u32, height: u32, ss_x: bool, ss_y: bool) -> [(usize, usize, usize, usize); 3] {
    let sb_cols = (width as usize).div_ceil(64);
    let sb_rows = (height as usize).div_ceil(64);
    let (aw, ah) = (sb_cols * 64, sb_rows * 64);
    let (sx, sy) = (ss_x as usize, ss_y as usize);
    let cw = (width as usize + sx) >> sx;
    let ch = (height as usize + sy) >> sy;
    [(aw, ah, width as usize, height as usize), (aw >> sx, ah >> sy, cw, ch), (aw >> sx, ah >> sy, cw, ch)]
}

/// Recycled buffers (strip / frame planes, mode info), so that steady-state decoding does not
/// allocate (and page-fault) megabytes per frame.
pub struct Pool<T> {
    bufs: std::sync::Mutex<Vec<Vec<T>>>,
    max: usize,
}

impl<T> Default for Pool<T> {
    fn default() -> Self {
        Pool::with_max(32)
    }
}

impl<T> Pool<T> {
    /// A pool keeping at most `max` buffers.
    pub fn with_max(max: usize) -> Self {
        Pool { bufs: Default::default(), max }
    }
}

impl<T: Clone> Pool<T> {
    /// A buffer of `len` elements with unspecified contents (recycled memory when available;
    /// `fill` only initialises fresh memory).
    ///
    /// Invariant for callers: every element that is read must have been written first. For the
    /// sample planes and mode info grids this holds because every sample inside the mode info grid
    /// is predicted before a later stage (loop filter, reference reads, intra edges) reads it, and
    /// every mode info position is written by its tile before it is read. Reads are clamped to
    /// that grid (intra edges to `max_x` / `max_y`, references to the visible size). A change that
    /// reads outside the grid would see stale data from a recycled buffer.
    pub fn take_any(&self, len: usize, fill: T) -> Vec<T> {
        let found = {
            let mut b = self.bufs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // Best fit by capacity: the first fit handed large buffers to small requests (band
            // scratch rows, chroma bands), and once truncated they never served a large one
            // again, so large requests kept allocating fresh (zeroed) memory.
            let mut best: Option<(usize, usize)> = None;
            for (i, v) in b.iter().enumerate() {
                let c = v.capacity();
                if c >= len && best.is_none_or(|(_, bc)| c < bc) {
                    best = Some((i, c));
                    if c == len {
                        break;
                    }
                }
            }
            best.map(|(i, _)| b.swap_remove(i))
        };
        match found {
            Some(mut v) => {
                // Contents are unspecified: only memory beyond the current length is written.
                if v.len() >= len {
                    v.truncate(len);
                } else {
                    v.resize(len, fill);
                }
                v
            }
            None => vec![fill; len],
        }
    }

    pub fn put(&self, v: Vec<T>) {
        if v.capacity() == 0 {
            return;
        }
        let mut b = self.bufs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if b.len() < self.max {
            b.push(v);
        }
    }
}

/// The decoder's buffer pools.
pub struct Pools {
    pub samples: Pool<u16>,
    /// Frame bands (many small buffers per frame).
    pub bands: Pool<u16>,
    pub mi: Pool<MiInfo>,
}

impl Default for Pools {
    fn default() -> Self {
        Pools { samples: Pool::default(), bands: Pool::with_max(1024), mi: Pool::default() }
    }
}
