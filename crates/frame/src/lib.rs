//! Video frames and audio buffers.
//!
//! - [`VideoFrame`]: decoded or rendered pictures. Planar Y'CbCr (8 or 16-bit containers) straight
//!   from decoders, sRGB-encoded RGBA8 from stills/generators, or linear premultiplied RGBA f32 in the
//!   compositor. Pixel data is `Arc`-shared so caches, monitors and export share frames for free.
//! - [`AudioBuffer`]: planar f32 samples.

use std::sync::Arc;

use filmcraft_color::{ColorInfo, DecodeTable, Matrix, Range, linear_to_srgb_u8, normalize_c, normalize_y, srgb_u8_to_linear_table, to_linear, ycbcr_to_rgb};
use filmcraft_time::Tick;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

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

    pub fn format_label(&self) -> String {
        match &self.data {
            PixelData::Rgba8(_) => "RGBA 8-bit".into(),
            PixelData::RgbaF32(_) => "RGBA 32-bit float".into(),
            PixelData::Yuv8 { chroma, .. } => format!("YUV {} 8-bit", chroma.label()),
            PixelData::Yuv16 { chroma, bits, .. } => format!("YUV {} {bits}-bit", chroma.label()),
        }
    }

    /// Approximate memory footprint in bytes (for caches).
    pub fn byte_size(&self) -> usize {
        match &self.data {
            PixelData::Rgba8(d) => d.len(),
            PixelData::RgbaF32(d) => d.len() * 4,
            PixelData::Yuv8 { planes, alpha, .. } => planes.iter().map(|p| p.len()).sum::<usize>() + alpha.as_ref().map_or(0, |a| a.len()),
            PixelData::Yuv16 { planes, alpha, .. } => (planes.iter().map(|p| p.len()).sum::<usize>() + alpha.as_ref().map_or(0, |a| a.len())) * 2,
        }
    }

    /// A planar Y'CbCr frame reduced by `n` (2, 4, 8…) in each direction: every sample (luma,
    /// chroma at its own resolution, alpha) is the rounded mean of an `n`×`n` block, chroma format
    /// and bit depth unchanged. Reduced-resolution playback hands the GPU this instead of the full
    /// picture (a quarter / sixteenth of the upload). None for RGBA frames or `n` < 2.
    pub fn box_decimated(&self, n: usize) -> Option<VideoFrame> {
        if n < 2 {
            return None;
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let (ow, oh) = ((w / n).max(1), (h / n).max(1));
        let data = match &self.data {
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

    /// The frame turned clockwise by `quarter_turns` × 90° (a container's display rotation).
    /// Planes rotate at their own resolution; 4:2:2 chroma is widened to 4:4:4 first for a quarter
    /// or three-quarter turn (its half-width chroma would become half-height, which has no
    /// [`Chroma`] variant). The pixel aspect ratio follows the turn.
    pub fn rotated(&self, quarter_turns: u8) -> VideoFrame {
        let q = quarter_turns % 4;
        if q == 0 {
            return self.clone();
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let data = match &self.data {
            PixelData::Rgba8(d) => PixelData::Rgba8(Arc::new(rotate_plane(d, w, h, 4, q))),
            PixelData::RgbaF32(d) => PixelData::RgbaF32(Arc::new(rotate_plane(d, w, h, 4, q))),
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (c, chroma) = rotate_chroma(planes, *chroma, w, h, q);
                PixelData::Yuv8 {
                    planes: [Arc::new(rotate_plane(&planes[0], w, h, 1, q)), Arc::new(c[0].clone()), Arc::new(c[1].clone())],
                    chroma,
                    alpha: alpha.as_ref().map(|a| Arc::new(rotate_plane(a, w, h, 1, q))),
                }
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (c, chroma) = rotate_chroma(planes, *chroma, w, h, q);
                PixelData::Yuv16 {
                    planes: [Arc::new(rotate_plane(&planes[0], w, h, 1, q)), Arc::new(c[0].clone()), Arc::new(c[1].clone())],
                    chroma,
                    bits: *bits,
                    alpha: alpha.as_ref().map(|a| Arc::new(rotate_plane(a, w, h, 1, q))),
                }
            }
        };
        let (width, height, par) = if q % 2 == 1 { (self.height, self.width, (self.par.1, self.par.0)) } else { (self.width, self.height, self.par) };
        VideoFrame { width, height, data, color: self.color, par, pts: self.pts }
    }

    /// Convert to premultiplied linear RGBA f32 (the compositor's working format).
    pub fn to_linear_f32(&self) -> Vec<f32> {
        self.to_linear_f32_decimated(1).2
    }

    /// Convert to premultiplied linear RGBA f32, box-filtering `n`×`n` blocks (n = 1, 2, 4, 8…)
    /// in linear light. Reduced-resolution playback uses this so it never builds full-size float
    /// buffers. Returns (width, height, pixels).
    pub fn to_linear_f32_decimated(&self, n: usize) -> (usize, usize, Vec<f32>) {
        self.to_linear_f32_decimated_with(n, None)
    }

    /// Like [`VideoFrame::to_linear_f32_decimated`], decoding each channel's signal through
    /// `decode` (a colour-managed curve: log, PQ, HLG scene light…) instead of the frame's
    /// transfer. Float frames are already linear and ignore it.
    pub fn to_linear_f32_decimated_with(&self, n: usize, decode: Option<&DecodeTable>) -> (usize, usize, Vec<f32>) {
        let n = n.max(1);
        let (w, h) = (self.width as usize, self.height as usize);
        let (ow, oh) = ((w / n).max(1), (h / n).max(1));
        let mut out = vec![0f32; ow * oh * 4];
        if let PixelData::RgbaF32(d) = &self.data
            && n == 1
        {
            out.copy_from_slice(d);
            return (ow, oh, out);
        }
        // Encoded (0..1, quantised to 12 bits) → linear lookup for this frame's transfer.
        let info = self.color;
        let lin: Vec<f32> = if decode.is_some() { Vec::new() } else { (0..4096).map(|i| to_linear(i as f32 / 4095.0, info.transfer)).collect() };
        let q = |v: f32| match decode {
            Some(t) => t.lookup(v),
            None => lin[(v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize],
        };
        let inv = 1.0 / (n * n) as f32;
        match &self.data {
            PixelData::RgbaF32(d) => {
                out.par_chunks_mut(ow * 4).enumerate().for_each(|(oy, row)| {
                    for ox in 0..ow {
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
                            row[ox * 4 + k] = acc[k] * inv;
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
                out.par_chunks_mut(ow * 4).enumerate().for_each(|(oy, row)| {
                    for ox in 0..ow {
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
                            row[ox * 4 + k] = acc[k] * inv;
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
                out.par_chunks_mut(ow * 4).enumerate().for_each(|(oy, row)| {
                    for ox in 0..ow {
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
                            row[ox * 4 + k] = acc[k] * inv;
                        }
                    }
                });
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (sx, sy) = chroma.shifts();
                let cw = w.div_ceil(1 << sx);
                let bits = *bits;
                let amax = ((1u32 << bits) - 1) as f32;
                out.par_chunks_mut(ow * 4).enumerate().for_each(|(oy, row)| {
                    for ox in 0..ow {
                        let mut acc = [0f32; 4];
                        for dy in 0..n {
                            let y = (oy * n + dy).min(h - 1);
                            let cy = y >> sy;
                            for dx in 0..n {
                                let x = (ox * n + dx).min(w - 1);
                                let cx = x >> sx;
                                let yy = normalize_y(planes[0][y * w + x] as u32, bits, info.range);
                                let u = normalize_c(planes[1][cy * cw + cx] as u32, bits, info.range);
                                let v = normalize_c(planes[2][cy * cw + cx] as u32, bits, info.range);
                                let rgb = ycbcr_to_rgb(yy, u, v, info.matrix);
                                let a = alpha.as_ref().map_or(1.0, |al| al[y * w + x] as f32 / amax);
                                acc[0] += q(rgb[0]) * a;
                                acc[1] += q(rgb[1]) * a;
                                acc[2] += q(rgb[2]) * a;
                                acc[3] += a;
                            }
                        }
                        for k in 0..4 {
                            row[ox * 4 + k] = acc[k] * inv;
                        }
                    }
                });
            }
        }
        (ow, oh, out)
    }

    /// Convert to straight-alpha sRGB RGBA8 for display (fast paths for 8-bit sources).
    pub fn to_rgba8(&self) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        match &self.data {
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
                let lin = self.to_linear_f32();
                let mut out = vec![0u8; w * h * 4];
                out.par_chunks_mut(w * 4).zip(lin.par_chunks(w * 4)).for_each(|(o, s)| linear_premul_to_srgb8(s, o));
                out
            }
        }
    }

    /// Luma plane (8-bit, for scopes/thumbnails analysis).
    pub fn luma8(&self) -> Vec<u8> {
        match &self.data {
            PixelData::Yuv8 { planes, .. } => planes[0].as_ref().clone(),
            _ => self.to_rgba8().chunks_exact(4).map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) as u8).collect(),
        }
    }
}

/// Convert a row of premultiplied linear f32 RGBA into straight sRGB RGBA8.
pub fn linear_premul_to_srgb8(src: &[f32], dst: &mut [u8]) {
    for (s, o) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
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
mod tests {

    #[test]
    fn rotation_turns_every_plane_clockwise() {
        // 4x2 RGBA8 with distinct pixels 0..8
        let px: Vec<u8> = (0..8u8).flat_map(|i| [i, 0, 0, 255]).collect();
        let f = VideoFrame { par: (4, 3), ..VideoFrame::rgba8(4, 2, px) };
        let r = f.rotated(1);
        assert_eq!((r.width, r.height, r.par), (2, 4, (3, 4)));
        let red: Vec<u8> = r.to_rgba8().chunks(4).map(|p| p[0]).collect();
        // source rows [0 1 2 3] / [4 5 6 7] turned clockwise: the bottom row becomes the left column
        assert_eq!(red, [4, 0, 5, 1, 6, 2, 7, 3]);
        let red180: Vec<u8> = f.rotated(2).to_rgba8().chunks(4).map(|p| p[0]).collect();
        assert_eq!(red180, [7, 6, 5, 4, 3, 2, 1, 0]);
        let red270: Vec<u8> = f.rotated(3).to_rgba8().chunks(4).map(|p| p[0]).collect();
        assert_eq!(red270, [3, 7, 2, 6, 1, 5, 0, 4]);
        assert_eq!(f.rotated(4).width, 4);

        // 4:2:0 4x2: chroma 2x1 -> 1x2; 4:2:2 4x2: chroma 2x2 -> widened 4:4:4 2x4
        let yuv = |chroma: Chroma, c: Vec<u8>| VideoFrame {
            width: 4,
            height: 2,
            data: PixelData::Yuv8 { planes: [Arc::new((0..8).collect()), Arc::new(c.clone()), Arc::new(c)], chroma, alpha: None },
            color: ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        };
        let r = yuv(Chroma::C420, vec![10, 20]).rotated(1);
        let PixelData::Yuv8 { planes, chroma, .. } = &r.data else { panic!() };
        assert_eq!((*chroma, &planes[0][..], &planes[1][..]), (Chroma::C420, &[4, 0, 5, 1, 6, 2, 7, 3][..], &[10, 20][..]));
        let r = yuv(Chroma::C422, vec![10, 20, 30, 40]).rotated(1);
        let PixelData::Yuv8 { planes, chroma, .. } = &r.data else { panic!() };
        assert_eq!((*chroma, &planes[1][..]), (Chroma::C444, &[30, 10, 30, 10, 40, 20, 40, 20][..]));
    }

    #[test]
    fn box_decimation_averages_blocks_per_plane() {
        // 6x4 4:2:0: luma ramp, chroma 3x2
        let y: Vec<u8> = (0..24).map(|i| (i * 10) as u8).collect();
        let u = vec![10, 20, 30, 40, 50, 60];
        let v = vec![200; 6];
        let f = VideoFrame {
            width: 6,
            height: 4,
            data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, alpha: None },
            color: ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        };
        let d = f.box_decimated(2).expect("yuv");
        assert_eq!((d.width, d.height), (3, 2));
        let PixelData::Yuv8 { planes, .. } = &d.data else { panic!() };
        // luma block (0,0): 0, 10, 60, 70 -> 35
        assert_eq!(planes[0][0], 35);
        assert_eq!(planes[0].len(), 6);
        // chroma 3x2 -> 2x1: blocks {10,20,40,50} = 30 and the clamped edge block {30,60} = 45
        assert_eq!(&planes[1][..], &[30, 45]);
        assert_eq!(&planes[2][..], &[200, 200]);
        assert!(f.box_decimated(1).is_none());
        assert!(VideoFrame::rgba8(2, 2, vec![0; 16]).box_decimated(2).is_none());
    }
    use super::*;

    #[test]
    fn rgba8_roundtrip_through_linear() {
        let px: Vec<u8> = (0..=255u8).flat_map(|v| [v, 255 - v, v / 2, 255]).collect();
        let f = VideoFrame::rgba8(256, 1, px.clone());
        let lin = f.to_linear_f32();
        let back = VideoFrame::rgba_f32(256, 1, lin).to_rgba8();
        assert_eq!(back, px);
    }

    #[test]
    fn yuv_grey_is_grey() {
        let (w, h) = (4, 2);
        let y = Arc::new(vec![126u8; w * h]);
        let u = Arc::new(vec![128u8; 2]);
        let v = Arc::new(vec![128u8; 2]);
        let f = VideoFrame {
            width: 4,
            height: 2,
            data: PixelData::Yuv8 { planes: [y, u, v], chroma: Chroma::C420, alpha: None },
            color: ColorInfo::REC709,
            par: (1, 1),
            pts: Tick::ZERO,
        };
        let rgb = f.to_rgba8();
        assert_eq!(&rgb[0..4], &[128, 128, 128, 255]);
        // slow path agrees within 1
        let slow = VideoFrame::rgba_f32(4, 2, f.to_linear_f32()).to_rgba8();
        assert!((slow[0] as i32 - 128).abs() <= 1);
    }

    #[test]
    fn audio_mix() {
        let mut a = AudioBuffer::silence(48000, 2, 4);
        let b = AudioBuffer { sample_rate: 48000, channels: vec![vec![0.5; 4]] };
        a.mix_from(&b, &[1.0, 0.5]);
        assert_eq!(a.channels[1][0], 0.25);
        assert_eq!(a.interleaved()[..2], [0.5, 0.25]);
    }
}
