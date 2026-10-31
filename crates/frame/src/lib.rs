//! Video frames and audio buffers.
//!
//! - [`VideoFrame`]: decoded or rendered pictures. Planar Y'CbCr (8 or 16-bit containers) straight
//!   from decoders, sRGB-encoded RGBA8 from stills/generators, or linear premultiplied RGBA f32 in the
//!   compositor. Pixel data is `Arc`-shared so caches, monitors and export share frames for free.
//! - [`AudioBuffer`]: planar f32 samples.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod memory;
pub mod pool;

pub use memory::{MemoryAccount, MemoryLease};

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use filmcraft_color::{ColorInfo, DecodeTable, Matrix, Range, linear_to_srgb_u8, normalize_c, normalize_y, srgb_u8_to_linear_table, to_linear, ycbcr_to_rgb};
use filmcraft_time::Tick;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Smallest full-size, opaque 8-bit Y'CbCr conversion that tabulates R' and B' per code pair
/// (two tables of 65 536 values): below it the tables cost more than they save.
const YUV8_PAIR_TABLE_MIN_PIXELS: usize = 1 << 18;

/// Chroma subsampling of planar Y'CbCr.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chroma {
    C420,
    C422,
    C444,
}

impl Chroma {
    /// (horizontal shift, vertical shift)
    pub fn shifts(self) -> (u32, u32) {
        match self {
            Chroma::C420 => (1, 1),
            Chroma::C422 => (1, 0),
            Chroma::C444 => (0, 0),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Chroma::C420 => "4:2:0",
            Chroma::C422 => "4:2:2",
            Chroma::C444 => "4:4:4",
        }
    }
}

/// GPU-backed decoded picture; CPU consumers use [`GpuPixels::cpu`] for one shared, fallible download.
pub trait GpuSurface: Send + Sync + std::fmt::Debug {
    /// Picture size in luma samples.
    fn size(&self) -> (u32, u32);
    /// GPU memory the picture occupies (cache accounting).
    fn byte_len(&self) -> usize;
    /// The picture in planar CPU form (`PixelData::Yuv8` / `Yuv16`).
    fn download(&self) -> Result<PixelData, String>;
    /// The concrete surface, for the GPU compositor to import.
    fn as_any(&self) -> &dyn std::any::Any;
    /// Process-unique id of the surface (upload cache key).
    fn id(&self) -> u64;
}

/// A [`GpuSurface`] with its chroma layout and bit depth, and the CPU copy once something asked
/// for it.
#[derive(Clone)]
pub struct GpuPixels {
    surface: Arc<dyn GpuSurface>,
    pub chroma: Chroma,
    /// Significant bits per sample (8 = NV12-like, 10 = P010-like).
    pub bits: u32,
    cpu: Arc<CpuState>,
}

struct CpuState {
    result: OnceLock<Result<PixelData, String>>,
    footprint: memory::Footprint,
}

impl std::fmt::Debug for GpuPixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuPixels").field("surface", &self.surface).field("chroma", &self.chroma).field("bits", &self.bits).finish()
    }
}

impl GpuPixels {
    pub fn new(surface: Arc<dyn GpuSurface>, chroma: Chroma, bits: u32) -> Self {
        let cpu = Arc::new(CpuState { result: OnceLock::new(), footprint: memory::Footprint::new(surface.byte_len()) });
        Self { surface, chroma, bits, cpu }
    }

    pub fn surface(&self) -> &Arc<dyn GpuSurface> {
        &self.surface
    }

    /// Download once; clones share successful pixels or a terminal error.
    pub fn cpu(&self) -> Result<&PixelData, String> {
        self.cpu
            .result
            .get_or_init(|| {
                let data = self.surface.download()?;
                if matches!(data, PixelData::Gpu(_)) {
                    return Err("GPU download returned another GPU surface".into());
                }
                self.cpu.footprint.add_cpu(data.byte_size());
                Ok(data)
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}

#[derive(Clone, Debug)]
pub enum PixelData {
    /// Straight-alpha, sRGB/709-encoded RGBA, 8 bits per channel.
    Rgba8(Arc<Vec<u8>>),
    /// Premultiplied, linear-light RGBA f32 (compositor working format).
    RgbaF32(Arc<Vec<f32>>),
    /// 8-bit planar Y'CbCr (optionally with an alpha plane).
    Yuv8 { planes: [Arc<Vec<u8>>; 3], chroma: Chroma, alpha: Option<Arc<Vec<u8>>> },
    /// 9–16-bit planar Y'CbCr stored in u16 (`bits` significant bits).
    Yuv16 { planes: [Arc<Vec<u16>>; 3], chroma: Chroma, bits: u32, alpha: Option<Arc<Vec<u16>>> },
    /// A planar Y'CbCr picture in GPU memory (see [`GpuSurface`]); CPU code uses
    /// [`VideoFrame::cpu`].
    Gpu(GpuPixels),
}

impl PixelData {
    fn byte_size(&self) -> usize {
        match self {
            Self::Gpu(g) => g.cpu.footprint.bytes(),
            Self::Rgba8(data) => data.len(),
            Self::RgbaF32(data) => data.len().saturating_mul(size_of::<f32>()),
            Self::Yuv8 { planes, alpha, .. } => planes.iter().chain(alpha).fold(0usize, |bytes, plane| bytes.saturating_add(plane.len())),
            Self::Yuv16 { planes, alpha, .. } => {
                planes.iter().chain(alpha).fold(0usize, |bytes, plane| bytes.saturating_add(plane.len().saturating_mul(size_of::<u16>())))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub data: PixelData,
    pub color: ColorInfo,
    /// Pixel aspect ratio (num, den).
    pub par: (u32, u32),
    /// Presentation time in media time (informational).
    pub pts: Tick,
}

/// A rectangle of pixels: `x`, `y` is its top-left corner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Region {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Region {
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    /// This rectangle cut to a `w`×`h` picture (empty when it lies outside).
    pub fn clip(self, w: usize, h: usize) -> Region {
        let x = self.x.min(w);
        let y = self.y.min(h);
        Region { x, y, w: self.w.min(w - x), h: self.h.min(h - y) }
    }
    /// Whether this rectangle is the whole `w`×`h` picture.
    pub fn is_full(&self, w: usize, h: usize) -> bool {
        (self.x, self.y, self.w, self.h) == (0, 0, w, h)
    }
}

/// First and last index of the items of `row` for which `on` holds.
fn row_extent<T>(row: &[T], on: impl Fn(&T) -> bool) -> Option<(usize, usize)> {
    let first = row.iter().position(&on)?;
    let last = row.iter().rposition(&on)?;
    Some((first, last))
}

impl VideoFrame {
    pub fn rgba8(width: u32, height: u32, data: Vec<u8>) -> Self {
        debug_assert_eq!(data.len(), (width * height * 4) as usize);
        Self { width, height, data: PixelData::Rgba8(Arc::new(data)), color: ColorInfo::SRGB_FULL, par: (1, 1), pts: Tick::ZERO }
    }
    pub fn rgba_f32(width: u32, height: u32, data: Vec<f32>) -> Self {
        debug_assert_eq!(data.len(), (width * height * 4) as usize);
        Self { width, height, data: PixelData::RgbaF32(Arc::new(data)), color: ColorInfo::SRGB_FULL, par: (1, 1), pts: Tick::ZERO }
    }
    pub fn transparent_f32(width: u32, height: u32) -> Self {
        Self::rgba_f32(width, height, vec![0.0; (width * height * 4) as usize])
    }
    pub fn with_pts(mut self, pts: Tick) -> Self {
        self.pts = pts;
        self
    }

    /// CPU pixels: borrow a CPU frame or share one cached GPU download; preserve metadata and propagate download failure.
    pub fn cpu(&self) -> Result<Cow<'_, VideoFrame>, String> {
        Ok(match &self.data {
            PixelData::Gpu(g) => {
                Cow::Owned(VideoFrame { width: self.width, height: self.height, data: g.cpu()?.clone(), color: self.color, par: self.par, pts: self.pts })
            }
            _ => Cow::Borrowed(self),
        })
    }

    pub fn format_label(&self) -> String {
        match &self.data {
            PixelData::Gpu(g) => format!("YUV {} {}-bit (GPU)", g.chroma.label(), g.bits),
            PixelData::Rgba8(_) => "RGBA 8-bit".into(),
            PixelData::RgbaF32(_) => "RGBA 32-bit float".into(),
            PixelData::Yuv8 { chroma, .. } => format!("YUV {} 8-bit", chroma.label()),
            PixelData::Yuv16 { chroma, bits, .. } => format!("YUV {} {bits}-bit", chroma.label()),
        }
    }

    /// Approximate memory footprint in bytes (for caches).
    pub fn byte_size(&self) -> usize {
        self.data.byte_size()
    }

    /// Charge this cache reference until its entry is dropped; GPU charges follow CPU completion.
    pub fn memory_charge(&self, account: &MemoryAccount) -> MemoryLease {
        match &self.data {
            PixelData::Gpu(g) => g.cpu.footprint.charge(account),
            _ => account.charge(self.byte_size()),
        }
    }

    /// A planar Y'CbCr frame reduced by `n` (2, 4, 8…) in each direction: every sample (luma,
    /// chroma at its own resolution, alpha) is the rounded mean of an `n`×`n` block, chroma format
    /// and bit depth unchanged. Reduced-resolution playback hands the GPU this instead of the full
    /// picture (a quarter / sixteenth of the upload). None for RGBA frames or `n` < 2.
    pub fn box_decimated(&self, n: usize) -> Option<VideoFrame> {
        let n = self.bounded_decimation(n);
        if n < 2 {
            return None;
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let (ow, oh) = ((w / n).max(1), (h / n).max(1));
        let data = match &self.data {
            // a GPU picture is minified by the GPU's sampler, not decimated on the CPU
            PixelData::Gpu(_) => return None,
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let (ocw, och) = (ow.div_ceil(1 << sx), oh.div_ceil(1 << sy));
                PixelData::Yuv8 {
                    planes: [
                        Arc::new(box_plane(&planes[0], w, h, ow, oh, n)),
                        Arc::new(box_plane(&planes[1], cw, ch, ocw, och, n)),
                        Arc::new(box_plane(&planes[2], cw, ch, ocw, och, n)),
                    ],
                    chroma: *chroma,
                    alpha: alpha.as_ref().map(|a| Arc::new(box_plane(a, w, h, ow, oh, n))),
                }
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let (ocw, och) = (ow.div_ceil(1 << sx), oh.div_ceil(1 << sy));
                PixelData::Yuv16 {
                    planes: [
                        Arc::new(box_plane(&planes[0], w, h, ow, oh, n)),
                        Arc::new(box_plane(&planes[1], cw, ch, ocw, och, n)),
                        Arc::new(box_plane(&planes[2], cw, ch, ocw, och, n)),
                    ],
                    chroma: *chroma,
                    bits: *bits,
                    alpha: alpha.as_ref().map(|a| Arc::new(box_plane(a, w, h, ow, oh, n))),
                }
            }
            PixelData::Rgba8(_) | PixelData::RgbaF32(_) => return None,
        };
        Some(VideoFrame { width: ow as u32, height: oh as u32, data, color: self.color, par: self.par, pts: self.pts })
    }

    /// Clip `region` to the frame; align Y'CbCr corners up/left to the chroma grid, keeping the size where possible.
    /// Empty/full regions preserve the frame; partial GPU crops materialize CPU pixels and propagate failure.
    pub fn cropped(&self, region: Region) -> Result<VideoFrame, String> {
        let (w, h) = (self.width as usize, self.height as usize);
        let (sx, sy) = match &self.data {
            PixelData::Gpu(g) => g.chroma.shifts(),
            PixelData::Yuv8 { chroma, .. } | PixelData::Yuv16 { chroma, .. } => chroma.shifts(),
            PixelData::Rgba8(_) | PixelData::RgbaF32(_) => (0, 0),
        };
        let (mx, my) = ((1usize << sx) - 1, (1usize << sy) - 1);
        let (x, y) = (region.x.min(w) & !mx, region.y.min(h) & !my);
        let r = Region { x, y, w: region.w, h: region.h }.clip(w, h);
        if r.w == 0 || r.h == 0 || (r.w == w && r.h == h) {
            return Ok(self.clone());
        }
        let chroma_rect = |cw: usize, ch: usize| {
            (r.x >> sx, r.y >> sy, r.w.div_ceil(1 << sx).min(cw - (r.x >> sx).min(cw)), r.h.div_ceil(1 << sy).min(ch - (r.y >> sy).min(ch)))
        };
        let data = match &self.data {
            PixelData::Gpu(_) => return self.cpu()?.cropped(region),
            PixelData::Rgba8(d) => PixelData::Rgba8(Arc::new(crop_plane(d, w, h, 4, r))),
            PixelData::RgbaF32(d) => PixelData::RgbaF32(Arc::new(crop_plane(d, w, h, 4, r))),
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let (cx, cy, cr_w, cr_h) = chroma_rect(cw, ch);
                let c = Region { x: cx, y: cy, w: cr_w, h: cr_h };
                PixelData::Yuv8 {
                    planes: [
                        Arc::new(crop_plane(&planes[0], w, h, 1, r)),
                        Arc::new(crop_plane(&planes[1], cw, ch, 1, c)),
                        Arc::new(crop_plane(&planes[2], cw, ch, 1, c)),
                    ],
                    chroma: *chroma,
                    alpha: alpha.as_ref().map(|a| Arc::new(crop_plane(a, w, h, 1, r))),
                }
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let (cx, cy, cr_w, cr_h) = chroma_rect(cw, ch);
                let c = Region { x: cx, y: cy, w: cr_w, h: cr_h };
                PixelData::Yuv16 {
                    planes: [
                        Arc::new(crop_plane(&planes[0], w, h, 1, r)),
                        Arc::new(crop_plane(&planes[1], cw, ch, 1, c)),
                        Arc::new(crop_plane(&planes[2], cw, ch, 1, c)),
                    ],
                    chroma: *chroma,
                    bits: *bits,
                    alpha: alpha.as_ref().map(|a| Arc::new(crop_plane(a, w, h, 1, r))),
                }
            }
        };
        Ok(VideoFrame { width: r.w as u32, height: r.h as u32, data, color: self.color, par: self.par, pts: self.pts })
    }

    /// The frame turned clockwise by `quarter_turns` × 90° (a container's display rotation).
    /// Planes rotate at their own resolution; 4:2:2 chroma is widened to 4:4:4 first for a quarter
    /// or three-quarter turn (its half-width chroma would become half-height, which has no
    /// [`Chroma`] variant). The pixel aspect ratio follows the turn.
    pub fn rotated(&self, quarter_turns: u8) -> Result<VideoFrame, String> {
        let q = quarter_turns % 4;
        if q == 0 {
            return Ok(self.clone());
        }
        if matches!(self.data, PixelData::Gpu(_)) {
            return self.cpu()?.rotated(quarter_turns);
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let data = match &self.data {
            PixelData::Rgba8(d) => PixelData::Rgba8(Arc::new(rotate_plane(d, w, h, 4, q))),
            PixelData::RgbaF32(d) => PixelData::RgbaF32(Arc::new(rotate_plane(d, w, h, 4, q))),
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let ([cb, cr], chroma) = rotate_chroma(planes, *chroma, w, h, q);
                PixelData::Yuv8 {
                    planes: [Arc::new(rotate_plane(&planes[0], w, h, 1, q)), Arc::new(cb), Arc::new(cr)],
                    chroma,
                    alpha: alpha.as_ref().map(|a| Arc::new(rotate_plane(a, w, h, 1, q))),
                }
            }
            PixelData::Gpu(_) => return self.cpu()?.rotated(quarter_turns),
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let ([cb, cr], chroma) = rotate_chroma(planes, *chroma, w, h, q);
                PixelData::Yuv16 {
                    planes: [Arc::new(rotate_plane(&planes[0], w, h, 1, q)), Arc::new(cb), Arc::new(cr)],
                    chroma,
                    bits: *bits,
                    alpha: alpha.as_ref().map(|a| Arc::new(rotate_plane(a, w, h, 1, q))),
                }
            }
        };
        let (width, height, par) = if q % 2 == 1 { (self.height, self.width, (self.par.1, self.par.0)) } else { (self.width, self.height, self.par) };
        Ok(VideoFrame { width, height, data, color: self.color, par, pts: self.pts })
    }

    /// Convert to premultiplied linear RGBA f32 (the compositor's working format).
    pub fn to_linear_f32(&self) -> Result<Vec<f32>, String> {
        Ok(self.to_linear_f32_decimated(1)?.2)
    }

    /// Convert to premultiplied linear RGBA f32, box-filtering `n`×`n` blocks (n = 1, 2, 4, 8…)
    /// in linear light. Reduced-resolution playback uses this so it never builds full-size float
    /// buffers. Returns (width, height, pixels).
    pub fn to_linear_f32_decimated(&self, n: usize) -> Result<(usize, usize, Vec<f32>), String> {
        self.to_linear_f32_decimated_with(n, None)
    }

    /// Like [`VideoFrame::to_linear_f32_decimated`], decoding each channel's signal through
    /// `decode` (a colour-managed curve: log, PQ, HLG scene light…) instead of the frame's
    /// transfer. Float frames are already linear and ignore it.
    pub fn to_linear_f32_decimated_with(&self, n: usize, decode: Option<&DecodeTable>) -> Result<(usize, usize, Vec<f32>), String> {
        if matches!(self.data, PixelData::Gpu(_)) {
            return self.cpu()?.to_linear_f32_decimated_with(n, decode);
        }
        let n = self.bounded_decimation(n);
        let (ow, oh) = self.decimated_size(n);
        // every element is written below, so a recycled buffer needs no zero-fill
        let mut out = pool::take_f32_overwritten(ow * oh * 4);
        self.convert_region(n, decode, Region { x: 0, y: 0, w: ow, h: oh }, &mut out)?;
        Ok((ow, oh, out))
    }

    /// The size of the picture after `n`×`n` box decimation.
    pub fn decimated_size(&self, n: usize) -> (usize, usize) {
        let n = self.bounded_decimation(n);
        ((self.width as usize / n).max(1), (self.height as usize / n).max(1))
    }

    /// [`VideoFrame::to_linear_f32_decimated_with`] for one rectangle of the decimated picture
    /// only: the rectangle (clipped to the picture) and its premultiplied linear RGBA pixels, row
    /// by row. Each pixel is exactly what the full conversion gives at that position. The buffer
    /// may come from the pool ([`pool::recycle_f32`] hands it back).
    pub fn to_linear_f32_region(&self, n: usize, decode: Option<&DecodeTable>, region: Region) -> Result<(Region, Vec<f32>), String> {
        if matches!(self.data, PixelData::Gpu(_)) {
            return self.cpu()?.to_linear_f32_region(n, decode, region);
        }
        let n = self.bounded_decimation(n);
        let (ow, oh) = self.decimated_size(n);
        let r = region.clip(ow, oh);
        let mut out = pool::take_f32_overwritten(r.w * r.h * 4);
        self.convert_region(n, decode, r, &mut out)?;
        Ok((r, out))
    }

    /// The picture after `n`×`n` box decimation ([`VideoFrame::decimated_size`]) as opaque sRGB
    /// RGBA8, for an opaque 8-bit Y'CbCr frame (`None` for any other): byte for byte what its linear
    /// picture ([`VideoFrame::to_linear_f32_decimated`]) gives through [`linear_to_srgb_u8`] with
    /// alpha 255, without the 16 bytes per pixel of floats in between. R' and B' are tabulated per
    /// code pair as the float conversion does (at full size already as bytes); G' goes through the
    /// table of its quantised signal.
    pub fn to_opaque_srgb8(&self, n: usize) -> Option<Vec<u8>> {
        let PixelData::Yuv8 { planes, chroma, alpha: None } = &self.data else { return None };
        let (w, h) = (self.width as usize, self.height as usize);
        let (sx, sy) = chroma.shifts();
        let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
        if w == 0 || h == 0 || planes[0].len() < w * h || planes[1].len() < cw * ch || planes[2].len() < cw * ch {
            return None;
        }
        let n = self.bounded_decimation(n);
        let (ow, oh) = self.decimated_size(n);
        let info = self.color;
        let lin: Vec<f32> = (0..4096).map(|i| to_linear(i as f32 / 4095.0, info.transfer)).collect();
        let at = |v: f32| (v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize;
        let ytab: Vec<f32> = (0..256).map(|v| normalize_y(v, 8, info.range)).collect();
        let ctab: Vec<f32> = (0..256).map(|v| normalize_c(v, 8, info.range)).collect();
        let (kr, kb) = info.matrix.kr_kb();
        let kg = 1.0 - kr - kb;
        let (cr_r, cb_b) = (2.0 * (1.0 - kr), 2.0 * (1.0 - kb));
        let (cr_g, cb_g) = (cr_r * kr / kg, cb_b * kb / kg);
        let pair =
            |f: &(dyn Fn(f32, f32) -> f32 + Sync)| -> Vec<f32> { (0..1usize << 16).into_par_iter().map(|i| lin[at(f(ytab[i >> 8], ctab[i & 255]))]).collect() };
        let rt = pair(&|yy, v| yy + cr_r * v);
        let bt = pair(&|yy, u| yy + cb_b * u);
        let mut out = vec![0u8; ow * oh * 4];
        if n == 1 {
            let bytes = |t: Vec<f32>| -> Vec<u8> { t.into_par_iter().map(linear_to_srgb_u8).collect() };
            let (rt, bt) = (bytes(rt), bytes(bt));
            let gt: Vec<u8> = lin.iter().map(|&l| linear_to_srgb_u8(l)).collect();
            out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                let cy = y >> sy;
                let (ys, us, vs) = (&planes[0][y * w..(y + 1) * w], &planes[1][cy * cw..(cy + 1) * cw], &planes[2][cy * cw..(cy + 1) * cw]);
                for (x, (o, &yc)) in row.as_chunks_mut::<4>().0.iter_mut().zip(ys).enumerate() {
                    let (yc, uc, vc) = (usize::from(yc), usize::from(us[x >> sx]), usize::from(vs[x >> sx]));
                    let (yy, u, v) = (ytab[yc], ctab[uc], ctab[vc]);
                    *o = [rt[yc << 8 | vc], gt[at(yy - cr_g * v - cb_g * u)], bt[yc << 8 | uc], 255];
                }
            });
            return Some(out);
        }
        // the float conversion's sums, in its order, each sample weighted by its alpha of 1
        let inv = 1.0 / (n as f32 * n as f32);
        out.par_chunks_mut(ow * 4).enumerate().for_each(|(oy, row)| {
            for (ox, o) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let mut acc = [0f32; 3];
                for dy in 0..n {
                    let y = (oy * n + dy).min(h - 1);
                    let cy = y >> sy;
                    for dx in 0..n {
                        let x = (ox * n + dx).min(w - 1);
                        let cx = x >> sx;
                        let (yc, uc, vc) = (usize::from(planes[0][y * w + x]), usize::from(planes[1][cy * cw + cx]), usize::from(planes[2][cy * cw + cx]));
                        let (yy, u, v) = (ytab[yc], ctab[uc], ctab[vc]);
                        acc[0] += rt[yc << 8 | vc] * 1.0;
                        acc[1] += lin[at(yy - cr_g * v - cb_g * u)] * 1.0;
                        acc[2] += bt[yc << 8 | uc] * 1.0;
                    }
                }
                *o = [linear_to_srgb_u8(acc[0] * inv), linear_to_srgb_u8(acc[1] * inv), linear_to_srgb_u8(acc[2] * inv), 255];
            }
        });
        Some(out)
    }
    /// Convert the rectangle `r` (inside the decimated picture) into `out`, which must hold exactly
    /// `r.w * r.h * 4` floats; every one of them is written.
    fn convert_region(&self, n: usize, decode: Option<&DecodeTable>, r: Region, out: &mut [f32]) -> Result<(), String> {
        if matches!(self.data, PixelData::Gpu(_)) {
            return self.cpu()?.convert_region(n, decode, r, out);
        }
        let (w, h) = (self.width as usize, self.height as usize);
        if r.w == 0 || r.h == 0 || out.len() != r.w * r.h * 4 {
            return Ok(());
        }
        if w == 0 || h == 0 {
            out.fill(0.0);
            return Ok(());
        }
        let n = n.min(w.max(h)).max(1);
        if let PixelData::RgbaF32(d) = &self.data
            && n == 1
        {
            for (ry, row) in out.chunks_exact_mut(r.w * 4).enumerate() {
                let start = ((r.y + ry) * w + r.x) * 4;
                match d.get(start..start + r.w * 4) {
                    Some(src) => row.copy_from_slice(src),
                    // a frame with less data than its size says: transparent, not what the buffer held before
                    None => row.fill(0.0),
                }
            }
            return Ok(());
        }
        // Encoded (0..1, quantised to 12 bits) → linear lookup for this frame's transfer.
        let info = self.color;
        let lin: Vec<f32> = if decode.is_some() { Vec::new() } else { (0..4096).map(|i| to_linear(i as f32 / 4095.0, info.transfer)).collect() };
        let q = |v: f32| match decode {
            Some(t) => t.lookup(v),
            None => lin[(v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize],
        };
        let inv = 1.0 / (n as f32 * n as f32);
        match &self.data {
            // (a GPU picture was handled above, through its CPU copy)
            PixelData::Gpu(_) => return Err("GPU surface was not materialized".into()),
            PixelData::RgbaF32(d) => {
                out.par_chunks_mut(r.w * 4).enumerate().for_each(|(ry, row)| {
                    let oy = r.y + ry;
                    for rx in 0..r.w {
                        let ox = r.x + rx;
                        let mut acc = [0f32; 4];
                        for dy in 0..n {
                            let y = (oy * n + dy).min(h - 1);
                            for dx in 0..n {
                                let x = (ox * n + dx).min(w - 1);
                                let i = (y * w + x) * 4;
                                for k in 0..4 {
                                    acc[k] += d[i + k];
                                }
                            }
                        }
                        for k in 0..4 {
                            row[rx * 4 + k] = acc[k] * inv;
                        }
                    }
                });
            }
            PixelData::Rgba8(d) => {
                let table: Vec<f32>;
                let lut: &[f32] = match decode {
                    Some(t) => {
                        table = (0..256).map(|i| t.lookup(i as f32 / 255.0)).collect();
                        &table
                    }
                    None => srgb_u8_to_linear_table(),
                };
                out.par_chunks_mut(r.w * 4).enumerate().for_each(|(ry, row)| {
                    let oy = r.y + ry;
                    for rx in 0..r.w {
                        let ox = r.x + rx;
                        let mut acc = [0f32; 4];
                        for dy in 0..n {
                            let y = (oy * n + dy).min(h - 1);
                            for dx in 0..n {
                                let x = (ox * n + dx).min(w - 1);
                                let s = &d[(y * w + x) * 4..(y * w + x) * 4 + 4];
                                let a = s[3] as f32 / 255.0;
                                acc[0] += lut[s[0] as usize] * a;
                                acc[1] += lut[s[1] as usize] * a;
                                acc[2] += lut[s[2] as usize] * a;
                                acc[3] += a;
                            }
                        }
                        for k in 0..4 {
                            row[rx * 4 + k] = acc[k] * inv;
                        }
                    }
                });
            }
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (sx, sy) = chroma.shifts();
                let cw = w.div_ceil(1 << sx);
                let ytab: Vec<f32> = (0..256).map(|v| normalize_y(v, 8, info.range)).collect();
                let ctab: Vec<f32> = (0..256).map(|v| normalize_c(v, 8, info.range)).collect();
                let (kr, kb) = info.matrix.kr_kb();
                let kg = 1.0 - kr - kb;
                let (cr_r, cb_b) = (2.0 * (1.0 - kr), 2.0 * (1.0 - kb));
                let (cr_g, cb_g) = (cr_r * kr / kg, cb_b * kb / kg);
                let ch = h.div_ceil(1 << sy);
                let fits = planes[0].len() >= w * h && planes[1].len() >= cw * ch && planes[2].len() >= cw * ch;
                if n == 1 && alpha.is_none() && fits && r.w * r.h >= YUV8_PAIR_TABLE_MIN_PIXELS {
                    // Full size and opaque (decoded video): R' depends on (Y', Cr) only and B' on
                    // (Y', Cb), so their decoded values are tabulated once for all 65 536 code
                    // pairs, with the very expressions the per-pixel loop below evaluates; G' is
                    // decoded per pixel. Each value is the loop's `(0 + q * 1) * inv` exactly.
                    let pair = |f: &(dyn Fn(f32, f32) -> f32 + Sync)| -> Vec<f32> {
                        (0..1usize << 16).into_par_iter().map(|i| f(ytab[i >> 8], ctab[i & 255])).collect()
                    };
                    let rt = pair(&|yy, v| (0.0 + q(yy + cr_r * v) * 1.0) * inv);
                    let bt = pair(&|yy, u| (0.0 + q(yy + cb_b * u) * 1.0) * inv);
                    let opaque = (0.0f32 + 1.0) * inv;
                    out.par_chunks_mut(r.w * 4).enumerate().for_each(|(ry, row)| {
                        let y = r.y + ry;
                        let cy = y >> sy;
                        let (Some(ys), Some(us), Some(vs)) =
                            (planes[0].get(y * w + r.x..y * w + r.x + r.w), planes[1].get(cy * cw..(cy + 1) * cw), planes[2].get(cy * cw..(cy + 1) * cw))
                        else {
                            row.fill(0.0);
                            return;
                        };
                        for (rx, (o, &yc)) in row.as_chunks_mut::<4>().0.iter_mut().zip(ys).enumerate() {
                            let cx = (r.x + rx) >> sx;
                            let (yc, uc, vc) = (usize::from(yc), us.get(cx).copied().map_or(0, usize::from), vs.get(cx).copied().map_or(0, usize::from));
                            let (yy, u, v) = (ytab[yc], ctab[uc], ctab[vc]);
                            o[0] = rt[yc << 8 | vc];
                            o[1] = (0.0 + q(yy - cr_g * v - cb_g * u) * 1.0) * inv;
                            o[2] = bt[yc << 8 | uc];
                            o[3] = opaque;
                        }
                    });
                    return Ok(());
                }
                out.par_chunks_mut(r.w * 4).enumerate().for_each(|(ry, row)| {
                    let oy = r.y + ry;
                    for rx in 0..r.w {
                        let ox = r.x + rx;
                        let mut acc = [0f32; 4];
                        for dy in 0..n {
                            let y = (oy * n + dy).min(h - 1);
                            let cy = y >> sy;
                            for dx in 0..n {
                                let x = (ox * n + dx).min(w - 1);
                                let cx = x >> sx;
                                let yy = ytab[planes[0][y * w + x] as usize];
                                let u = ctab[planes[1][cy * cw + cx] as usize];
                                let v = ctab[planes[2][cy * cw + cx] as usize];
                                let a = alpha.as_ref().map_or(1.0, |al| al[y * w + x] as f32 / 255.0);
                                acc[0] += q(yy + cr_r * v) * a;
                                acc[1] += q(yy - cr_g * v - cb_g * u) * a;
                                acc[2] += q(yy + cb_b * u) * a;
                                acc[3] += a;
                            }
                        }
                        for k in 0..4 {
                            row[rx * 4 + k] = acc[k] * inv;
                        }
                    }
                });
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (sx, sy) = chroma.shifts();
                let cw = w.div_ceil(1 << sx);
                let bits = *bits;
                let amax = ((1u32 << bits) - 1) as f32;
                // Per-code tables (exactly the values `normalize_*` returns) instead of two
                // divisions per sample; a code past the table (a corrupt plane) is computed.
                let codes = if bits <= 12 { 1usize << bits } else { 0 };
                let ytab: Vec<f32> = (0..codes as u32).map(|v| normalize_y(v, bits, info.range)).collect();
                let ctab: Vec<f32> = (0..codes as u32).map(|v| normalize_c(v, bits, info.range)).collect();
                let ny = |v: u16| ytab.get(usize::from(v)).copied().unwrap_or_else(|| normalize_y(u32::from(v), bits, info.range));
                let nc = |v: u16| ctab.get(usize::from(v)).copied().unwrap_or_else(|| normalize_c(u32::from(v), bits, info.range));
                out.par_chunks_mut(r.w * 4).enumerate().for_each(|(ry, row)| {
                    let oy = r.y + ry;
                    for rx in 0..r.w {
                        let ox = r.x + rx;
                        let mut acc = [0f32; 4];
                        for dy in 0..n {
                            let y = (oy * n + dy).min(h - 1);
                            let cy = y >> sy;
                            for dx in 0..n {
                                let x = (ox * n + dx).min(w - 1);
                                let cx = x >> sx;
                                let yy = ny(planes[0][y * w + x]);
                                let u = nc(planes[1][cy * cw + cx]);
                                let v = nc(planes[2][cy * cw + cx]);
                                let rgb = ycbcr_to_rgb(yy, u, v, info.matrix);
                                let a = alpha.as_ref().map_or(1.0, |al| al[y * w + x] as f32 / amax);
                                acc[0] += q(rgb[0]) * a;
                                acc[1] += q(rgb[1]) * a;
                                acc[2] += q(rgb[2]) * a;
                                acc[3] += a;
                            }
                        }
                        for k in 0..4 {
                            row[rx * 4 + k] = acc[k] * inv;
                        }
                    }
                });
            }
        }
        Ok(())
    }

    /// Where the picture is not transparent: the smallest rectangle of the `n`×`n`-decimated
    /// picture that holds every pixel whose alpha is not zero (empty for a picture that is
    /// transparent everywhere). `None` when the frame has no alpha plane to look at (it is opaque,
    /// or its alpha is not stored as integers), so nothing can be skipped. Pixels outside the
    /// rectangle have alpha exactly 0 and, in the premultiplied conversion, colour 0 as well.
    pub fn alpha_region(&self, n: usize) -> Option<Region> {
        let n = self.bounded_decimation(n);
        let (w, h) = (self.width as usize, self.height as usize);
        let px = w.checked_mul(h)?;
        // the first and last x of non-transparent pixels in source row `y`
        let extent: Box<dyn Fn(usize) -> Option<(usize, usize)> + Sync + '_> = match &self.data {
            PixelData::Yuv8 { alpha: Some(a), .. } if a.len() == px => Box::new(move |y| row_extent(&a[y * w..(y + 1) * w], |v| *v != 0)),
            PixelData::Yuv16 { alpha: Some(a), .. } if a.len() == px => Box::new(move |y| row_extent(&a[y * w..(y + 1) * w], |v| *v != 0)),
            PixelData::Rgba8(d) if d.len() == px.checked_mul(4)? => {
                Box::new(move |y| row_extent(d[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0, |p: &[u8; 4]| p[3] != 0))
            }
            _ => return None,
        };
        // (first row, last row, first column, last column) over the rows that have any
        let found = (0..h)
            .into_par_iter()
            .filter_map(|y| extent(y).map(|(a, b)| (y, y, a, b)))
            .reduce_with(|p, q| (p.0.min(q.0), p.1.max(q.1), p.2.min(q.2), p.3.max(q.3)));
        let (ow, oh) = self.decimated_size(n);
        let Some((y0, y1, x0, x1)) = found else { return Some(Region { x: 0, y: 0, w: 0, h: 0 }) };
        // a decimated pixel covers source pixels [o·n, o·n + n)
        let (rx0, ry0) = (x0 / n, y0 / n);
        let (rx1, ry1) = (x1 / n + 1, y1 / n + 1);
        Some(Region { x: rx0, y: ry0, w: rx1.saturating_sub(rx0), h: ry1.saturating_sub(ry0) }.clip(ow, oh))
    }

    /// Keep hostile decimation requests from doing work beyond the source picture.  Larger boxes
    /// only resample the edge pixels, so clamping them has no useful effect on the reduced output.
    fn bounded_decimation(&self, n: usize) -> usize {
        n.clamp(1, (self.width as usize).max(self.height as usize).max(1))
    }

    /// Convert to straight-alpha sRGB RGBA8 for display (fast paths for 8-bit sources).
    pub fn to_rgba8(&self) -> Result<Vec<u8>, String> {
        if matches!(self.data, PixelData::Gpu(_)) {
            return self.cpu()?.to_rgba8();
        }
        let (w, h) = (self.width as usize, self.height as usize);
        Ok(match &self.data {
            PixelData::Rgba8(d) => d.as_ref().clone(),
            PixelData::Yuv8 { planes, chroma, alpha: None }
                if matches!(self.color.transfer, filmcraft_color::Transfer::Bt709 | filmcraft_color::Transfer::Srgb) =>
            {
                // Direct display path: Y'CbCr → R'G'B' without linearisation.
                let (sx, sy) = chroma.shifts();
                let cw = w.div_ceil(1 << sx);
                let (kr, kb) = self.color.matrix.kr_kb();
                let kg = 1.0 - kr - kb;
                let (ys, yo, cs) = match self.color.range {
                    Range::Limited => (255.0 / 219.0, 16.0, 255.0 / 224.0),
                    Range::Full => (1.0, 0.0, 1.0),
                };
                let (crr, cbb) = (2.0 * (1.0 - kr) * cs, 2.0 * (1.0 - kb) * cs);
                let (cgr, cgb) = (crr * kr / kg, cbb * kb / kg);
                let mut out = vec![0u8; w * h * 4];
                out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    let cy = y >> sy;
                    let yrow = &planes[0][y * w..y * w + w];
                    let urow = &planes[1][cy * cw..cy * cw + cw];
                    let vrow = &planes[2][cy * cw..cy * cw + cw];
                    for x in 0..w {
                        let yy = (yrow[x] as f32 - yo) * ys;
                        let u = urow[x >> sx] as f32 - 128.0;
                        let v = vrow[x >> sx] as f32 - 128.0;
                        let o = &mut row[x * 4..x * 4 + 4];
                        o[0] = (yy + crr * v).round().clamp(0.0, 255.0) as u8;
                        o[1] = (yy - cgr * v - cgb * u).round().clamp(0.0, 255.0) as u8;
                        o[2] = (yy + cbb * u).round().clamp(0.0, 255.0) as u8;
                        o[3] = 255;
                    }
                });
                out
            }
            _ => {
                let lin = self.to_linear_f32()?;
                let mut out = vec![0u8; w * h * 4];
                out.par_chunks_mut(w * 4).zip(lin.par_chunks(w * 4)).for_each(|(o, s)| linear_premul_to_srgb8(s, o));
                out
            }
        })
    }

    /// Luma plane (8-bit, for scopes/thumbnails analysis).
    pub fn luma8(&self) -> Result<Vec<u8>, String> {
        if matches!(self.data, PixelData::Gpu(_)) {
            return self.cpu()?.luma8();
        }
        Ok(match &self.data {
            PixelData::Yuv8 { planes, .. } => planes[0].as_ref().clone(),
            _ => self.to_rgba8()?.as_chunks::<4>().0.iter().map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) as u8).collect(),
        })
    }
}

/// Convert a row of premultiplied linear f32 RGBA into straight sRGB RGBA8.
pub fn linear_premul_to_srgb8(src: &[f32], dst: &mut [u8]) {
    for (s, o) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0) {
        let a = s[3].clamp(0.0, 1.0);
        if a <= 0.0 {
            o.copy_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let inv = 1.0 / a;
        o[0] = linear_to_srgb_u8(s[0] * inv);
        o[1] = linear_to_srgb_u8(s[1] * inv);
        o[2] = linear_to_srgb_u8(s[2] * inv);
        o[3] = (a * 255.0 + 0.5) as u8;
    }
}

/// Planar f32 audio.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioBuffer {
    pub sample_rate: u32,
    /// One Vec per channel, all the same length.
    pub channels: Vec<Vec<f32>>,
}

impl AudioBuffer {
    pub fn silence(sample_rate: u32, channels: usize, frames: usize) -> Self {
        Self { sample_rate, channels: vec![vec![0.0; frames]; channels] }
    }
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }
    /// Mix `other` into self with gain (channel counts are matched by wrapping/mono-spreading).
    pub fn mix_from(&mut self, other: &AudioBuffer, gains: &[f32]) {
        let n = self.frames().min(other.frames());
        for (c, dst) in self.channels.iter_mut().enumerate() {
            let src = &other.channels[if other.channels.len() == 1 { 0 } else { c % other.channels.len() }];
            let g = gains.get(c).copied().unwrap_or(1.0);
            for i in 0..n {
                dst[i] += src[i] * g;
            }
        }
    }
    /// Interleave into a single Vec (for audio output / encoders).
    pub fn interleaved(&self) -> Vec<f32> {
        let n = self.frames();
        let c = self.channels.len();
        let mut out = vec![0.0; n * c];
        for (ci, ch) in self.channels.iter().enumerate() {
            for (i, s) in ch.iter().enumerate() {
                out[i * c + ci] = *s;
            }
        }
        out
    }
    /// Peak absolute sample per channel.
    pub fn peaks(&self) -> Vec<f32> {
        self.channels.iter().map(|c| c.iter().fold(0f32, |m, s| m.max(s.abs()))).collect()
    }
}

/// Matrix used when a decoder does not signal one: BT.601 for SD, BT.709 otherwise (common practice).
pub fn default_matrix(width: u32, height: u32) -> Matrix {
    if width <= 1024 && height <= 576 { Matrix::Bt601 } else { Matrix::Bt709 }
}

/// `n`×`n` box mean of a `w`×`h` plane into `ow`×`oh` (blocks clamped at the right / bottom).
fn box_plane<T: Copy + Into<u32> + TryFrom<u32> + Send + Sync + Default>(src: &[T], w: usize, h: usize, ow: usize, oh: usize, n: usize) -> Vec<T> {
    let mut out = vec![T::default(); ow * oh];
    if w == 0 || h == 0 || src.len() < w * h {
        return out;
    }
    out.par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
        let mut acc = vec![0u32; ow];
        let mut cnt = vec![0u32; ow];
        for y in (oy * n..(oy + 1) * n).filter(|&y| y < h) {
            let line = &src[y * w..y * w + w];
            for ((a, c), chunk) in acc.iter_mut().zip(cnt.iter_mut()).zip(line.chunks(n)) {
                *a += chunk.iter().map(|&v| v.into()).sum::<u32>();
                *c += chunk.len() as u32;
            }
        }
        for ((o, a), c) in row.iter_mut().zip(&acc).zip(&cnt) {
            let c = (*c).max(1);
            *o = T::try_from((a + c / 2) / c).unwrap_or_default();
        }
    });
    out
}

/// The `r` part of a `w`×`h` plane of `n`-element pixels (`r` lies inside the plane). A plane too
/// short for its size gives a blank result, as in [`rotate_plane`].
fn crop_plane<T: Copy + Default>(src: &[T], w: usize, h: usize, n: usize, r: Region) -> Vec<T> {
    let mut out = vec![T::default(); r.w * r.h * n];
    if src.len() < w * h * n || r.x + r.w > w || r.y + r.h > h {
        return out;
    }
    for (row, o) in out.chunks_exact_mut((r.w * n).max(1)).enumerate().take(r.h) {
        let s = ((r.y + row) * w + r.x) * n;
        o.copy_from_slice(&src[s..s + r.w * n]);
    }
    out
}

/// A `w`×`h` plane of `n`-element pixels turned clockwise by `q` (1–3) quarter turns.
fn rotate_plane<T: Copy + Default + Send + Sync>(src: &[T], w: usize, h: usize, n: usize, q: u8) -> Vec<T> {
    let mut out = vec![T::default(); w * h * n];
    if w == 0 || h == 0 || src.len() < w * h * n {
        return out;
    }
    // output width: h for a quarter / three-quarter turn
    let ow = if q % 2 == 1 { h } else { w };
    out.par_chunks_mut(ow * n).enumerate().for_each(|(oy, row)| {
        for ox in 0..ow {
            // the source pixel shown at (ox, oy)
            let (x, y) = match q {
                1 => (oy, h - 1 - ox),
                2 => (w - 1 - ox, h - 1 - oy),
                _ => (w - 1 - oy, ox),
            };
            let s = (y * w + x) * n;
            row[ox * n..ox * n + n].copy_from_slice(&src[s..s + n]);
        }
    });
    out
}

/// The two chroma planes of a `w`×`h` picture turned by `q` quarter turns, and their new format.
fn rotate_chroma<T: Copy + Default + Send + Sync>(planes: &[Arc<Vec<T>>; 3], chroma: Chroma, w: usize, h: usize, q: u8) -> ([Vec<T>; 2], Chroma) {
    let (sx, sy) = chroma.shifts();
    let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
    if chroma == Chroma::C422 && q % 2 == 1 {
        // widen to 4:4:4 (each chroma sample covers two luma columns), then turn
        let widen = |p: &[T]| -> Vec<T> {
            let mut o = vec![T::default(); w * h];
            if p.len() >= cw * h {
                for y in 0..h {
                    for x in 0..w {
                        o[y * w + x] = p[y * cw + x / 2];
                    }
                }
            }
            o
        };
        return ([rotate_plane(&widen(&planes[1]), w, h, 1, q), rotate_plane(&widen(&planes[2]), w, h, 1, q)], Chroma::C444);
    }
    ([rotate_plane(&planes[1], cw, ch, 1, q), rotate_plane(&planes[2], cw, ch, 1, q)], chroma)
}

#[cfg(test)]
mod tests;
