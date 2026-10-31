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
/// top of a frame while its bottom is still being filtered (frame threading). A frame nothing
/// reads before it is complete is instead finished in whole planes and read in place, rows
/// `strides[p]` apart, without band lookups.
pub struct Frame {
    pub info: FrameInfo,
    /// Allocated plane widths (whole superblocks) = row strides.
    pub strides: [usize; 3],
    /// Rows per band of each plane.
    pub band_h: [usize; 3],
    /// Visible (cropped) plane sizes.
    pub vis: [(usize, usize); 3],
    bands: Box<[std::sync::OnceLock<Band>]>,
    /// The whole planes of a frame finished in place (set before its bands are published).
    full: std::sync::OnceLock<[Vec<u16>; 3]>,
    /// Finished in bands, readable before it is complete; else in whole planes.
    keep_bands: bool,
    pools: std::sync::Arc<Pools>,
}

impl Frame {
    /// A frame to be finished in bands, readable before it is complete (`keep_bands`), or in whole
    /// planes ([`Frame::complete`]).
    pub fn new(info: FrameInfo, pools: std::sync::Arc<Pools>, keep_bands: bool) -> Frame {
        let geo = plane_geometry(info.width, info.height, info.ss_x, info.ss_y);
        let sb_rows = (info.height as usize).div_ceil(64);
        Frame {
            info,
            strides: geo.map(|g| g.0),
            band_h: [64, 64 >> info.ss_y as usize, 64 >> info.ss_y as usize],
            vis: geo.map(|g| (g.2, g.3)),
            bands: (0..sb_rows).map(|_| std::sync::OnceLock::new()).collect(),
            full: std::sync::OnceLock::new(),
            keep_bands,
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

    /// Whether the frame is finished in bands (see [`Frame::new`]).
    pub fn keeps_bands(&self) -> bool {
        self.keep_bands
    }

    /// Complete the frame from its whole planes (every band published without samples).
    pub fn complete(&self, planes: [Vec<u16>; 3]) {
        if let Err(planes) = self.full.set(planes) {
            for p in planes {
                self.pools.samples.put(p);
            }
        }
        for b in self.bands.iter() {
            let _ = b.set(Band { planes: Default::default() });
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

    /// Plane `p` in one piece (rows `strides[p]` apart) when the frame was finished in place.
    #[inline]
    pub fn whole(&self, p: usize) -> Option<&[u16]> {
        // A frame finished in bands has none: skip the (acquire) load.
        if self.keep_bands {
            return None;
        }
        self.full.get().map(|f| f[p].as_slice())
    }

    /// Row `y` of plane `p` (all `strides[p]` samples), waiting until it is final.
    #[inline]
    pub fn row(&self, p: usize, y: usize) -> &[u16] {
        match self.whole(p) {
            Some(w) => w.get(y * self.strides[p]..(y + 1) * self.strides[p]).unwrap_or_default(),
            None => self.band_row(p, y),
        }
    }

    /// Row `y` of plane `p` of a frame finished in bands.
    #[inline]
    pub fn band_row(&self, p: usize, y: usize) -> &[u16] {
        let bh = self.band_h[p];
        let s = self.strides[p];
        let o = (y % bh + BAND_PAD) * s;
        self.band(y / bh).planes[p].get(o..o + s).unwrap_or_default()
    }

    /// Rows `y0..y1` of plane `p` as one slice with stride `strides[p]`: of the whole planes, or
    /// of a band when they lie in one: (samples, offset of row y0).
    #[inline]
    pub fn span(&self, p: usize, y0: usize, y1: usize) -> Option<(&[u16], usize)> {
        if let Some(w) = self.whole(p) {
            return Some((w, y0 * self.strides[p]));
        }
        let bh = self.band_h[p];
        let r = y0 / bh;
        if (y1 - 1) / bh != r {
            return None;
        }
        Some((&self.band(r).planes[p], (y0 % bh + BAND_PAD) * self.strides[p]))
    }

    /// A frame finished in whole planes (tests): `planes` sized as the frame's.
    #[cfg(test)]
    pub fn from_whole_planes(info: FrameInfo, planes: [&[u16]; 3]) -> Frame {
        let f = Frame::new(info, Default::default(), false);
        f.complete(planes.map(|p| p.to_vec()));
        f
    }

    /// A complete frame from full planes (tests).
    #[cfg(test)]
    pub fn from_planes(info: FrameInfo, planes: [&[u16]; 3]) -> Frame {
        let f = Frame::new(info, Default::default(), true);
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
        for p in self.full.take().into_iter().flatten() {
            self.pools.samples.put(p);
        }
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
    pub fn take_any(&self, len: usize, fill: T) -> Vec<T> {
        match self.best_fit(len) {
            Some(mut v) => {
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

    /// A buffer of `len` copies of `fill` (recycled memory when available).
    pub fn take(&self, len: usize, fill: T) -> Vec<T> {
        match self.best_fit(len) {
            Some(mut v) => {
                v.clear();
                v.resize(len, fill);
                v
            }
            None => vec![fill; len],
        }
    }

    /// The pooled buffer of the least capacity of at least `len`, so that buffers of one size
    /// (a plane's band, say) keep serving that size rather than being cut down for smaller ones.
    fn best_fit(&self, len: usize) -> Option<Vec<T>> {
        let mut b = self.bufs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let i = b.iter().enumerate().filter(|(_, v)| v.capacity() >= len).min_by_key(|(_, v)| v.capacity()).map(|(i, _)| i)?;
        Some(b.swap_remove(i))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Buffers of two sizes taken and returned in turns (a luma and a chroma band, say) are each
    /// reused at their own size: the large one is not cut down for the small request and lost
    /// for the next large one.
    #[test]
    fn pool_reuses_buffers_by_best_fit() {
        let pool = Pool::<u16>::default();
        pool.put(vec![0; 1000]);
        pool.put(vec![0; 250]);
        for _ in 0..3 {
            let small = pool.take_any(250, 0);
            let large = pool.take_any(1000, 0);
            assert_eq!((small.capacity(), large.capacity()), (250, 1000));
            assert_eq!((small.len(), large.len()), (250, 1000));
            pool.put(small);
            pool.put(large);
        }
        // A smaller request served by the only buffer keeps its capacity for a later full one.
        let only = Pool::<u16>::default();
        only.put(vec![0; 1000]);
        let v = only.take_any(10, 0);
        assert_eq!((v.len(), v.capacity()), (10, 1000));
        only.put(v);
        let v = only.take_any(1000, 7);
        assert_eq!((v.len(), v.capacity()), (1000, 1000));
        let v = only.take(5, 3);
        assert_eq!(v, vec![3; 5]);
    }
}
