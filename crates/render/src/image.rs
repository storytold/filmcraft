//! The compositor's working image: premultiplied linear-light RGBA f32, row-major.

use filmcraft_geom::Affine;
use rayon::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Image {
    pub const MAX_PIXELS: usize = 268_435_456;

    pub fn new(w: usize, h: usize) -> Self {
        let len = w.checked_mul(h).filter(|&px| px <= Self::MAX_PIXELS).and_then(|px| px.checked_mul(4)).unwrap_or(0);
        let (w, h, px) = if len == 0 && (w > 0 || h > 0) {
            (0, 0, Vec::new())
        } else {
            let mut px = Vec::new();
            if px.try_reserve_exact(len).is_ok() {
                px.resize(len, 0.0);
                (w, h, px)
            } else {
                (0, 0, Vec::new())
            }
        };
        Self { w, h, px }
    }
    pub fn filled(w: usize, h: usize, c: [f32; 4]) -> Self {
        let len = w.checked_mul(h).filter(|&px| px <= Self::MAX_PIXELS).and_then(|px| px.checked_mul(4)).unwrap_or(0);
        let (w, h, px) = if len == 0 && (w > 0 || h > 0) {
            (0, 0, Vec::new())
        } else {
            let mut px = Vec::new();
            if px.try_reserve_exact(len).is_ok() {
                for _ in 0..w * h {
                    px.extend_from_slice(&c);
                }
                (w, h, px)
            } else {
                (0, 0, Vec::new())
            }
        };
        Self { w, h, px }
    }
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> [f32; 4] {
        let i = (y * self.w + x) * 4;
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }
    /// Clamp-to-edge fetch.
    #[inline]
    pub fn get_clamped(&self, x: isize, y: isize) -> [f32; 4] {
        let x = x.clamp(0, self.w as isize - 1) as usize;
        let y = y.clamp(0, self.h as isize - 1) as usize;
        self.get(x, y)
    }
    /// Transparent-border fetch.
    #[inline]
    pub fn get_or_clear(&self, x: isize, y: isize) -> [f32; 4] {
        if x < 0 || y < 0 || x >= self.w as isize || y >= self.h as isize { [0.0; 4] } else { self.get(x as usize, y as usize) }
    }
    /// Bilinear sample at continuous pixel coords (pixel centres at +0.5), transparent outside.
    #[inline]
    pub fn sample_bilinear(&self, x: f32, y: f32) -> [f32; 4] {
        let fx = x - 0.5;
        let fy = y - 0.5;
        let x0 = fx.floor();
        let y0 = fy.floor();
        let tx = fx - x0;
        let ty = fy - y0;
        let (x0, y0) = (x0 as isize, y0 as isize);
        let a = self.get_or_clear(x0, y0);
        let b = self.get_or_clear(x0 + 1, y0);
        let c = self.get_or_clear(x0, y0 + 1);
        let d = self.get_or_clear(x0 + 1, y0 + 1);
        let mut o = [0.0; 4];
        for k in 0..4 {
            let top = a[k] + (b[k] - a[k]) * tx;
            let bot = c[k] + (d[k] - c[k]) * tx;
            o[k] = top + (bot - top) * ty;
        }
        o
    }
    #[inline]
    pub fn sample_bilinear_clamped(&self, x: f32, y: f32) -> [f32; 4] {
        let fx = (x - 0.5).clamp(0.0, self.w as f32 - 1.0);
        let fy = (y - 0.5).clamp(0.0, self.h as f32 - 1.0);
        let x0 = fx.floor() as usize;
        let y0 = fy.floor() as usize;
        let x1 = (x0 + 1).min(self.w - 1);
        let y1 = (y0 + 1).min(self.h - 1);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let (a, b, c, d) = (self.get(x0, y0), self.get(x1, y0), self.get(x0, y1), self.get(x1, y1));
        let mut o = [0.0; 4];
        for k in 0..4 {
            let top = a[k] + (b[k] - a[k]) * tx;
            let bot = c[k] + (d[k] - c[k]) * tx;
            o[k] = top + (bot - top) * ty;
        }
        o
    }

    /// Resample `self` into a `w`×`h` image through `m` (maps source pixels → destination pixels).
    /// Uses bilinear filtering, with a box pre-filter (mip level) when minifying by more than 2×.
    /// A placement that only scales and moves the picture takes [`AxisResample`], which gives the
    /// same pixels.
    pub fn transformed(&self, w: usize, h: usize, m: &Affine) -> Image {
        if let Some(rs) = AxisResample::new(self.w, self.h, w, h, m)
            && self.px.len() == self.w * self.h * 4
        {
            return rs.apply(self, (0, 0));
        }
        self.transformed_sampled(w, h, m)
    }

    /// [`Image::transformed`] one destination pixel at a time, for any placement: the reference
    /// [`AxisResample`] is compared with bit for bit.
    pub(crate) fn transformed_sampled(&self, w: usize, h: usize, m: &Affine) -> Image {
        if self.w == 0 || self.h == 0 || w == 0 || h == 0 {
            return Image::new(w, h);
        }
        let Some(inv) = m.inverse() else { return Image::new(w, h) };
        // minification factor
        let sx = (m.a * m.a + m.b * m.b).sqrt();
        let sy = (m.c * m.c + m.d * m.d).sqrt();
        let minify = 1.0 / sx.min(sy).max(1e-6);
        if minify >= 2.0 && self.w >= 4 && self.h >= 4 {
            let half = self.downsample2();
            let m2 = m.then_apply(&Affine::scale(2.0, 2.0));
            return half.transformed(w, h, &m2);
        }
        if w == 0 || h == 0 {
            return Image::new(0, 0);
        }
        let mut out = Image::new(w, h);
        // Destination bounds of the source rect, to skip empty rows/cols.
        let b = m.bounds(&filmcraft_geom::Rect::new(0.0, 0.0, self.w as f64, self.h as f64));
        let y0 = (b.y.floor().max(0.0) as usize).min(h);
        let y1 = (b.bottom().ceil().max(0.0) as usize).min(h);
        let x0 = (b.x.floor().max(0.0) as usize).min(w);
        let x1 = (b.right().ceil().max(0.0) as usize).min(w);
        let axis_aligned = m.b == 0.0 && m.c == 0.0;
        out.px.par_chunks_mut(w * 4).enumerate().skip(y0).take(y1.saturating_sub(y0)).for_each(|(y, row)| {
            let py = y as f64 + 0.5;
            for x in x0..x1 {
                let px = x as f64 + 0.5;
                let (u, v) =
                    if axis_aligned { (inv.a * px + inv.e, inv.d * py + inv.f) } else { (inv.a * px + inv.c * py + inv.e, inv.b * px + inv.d * py + inv.f) };
                if u < -1.0 || v < -1.0 || u > self.w as f64 + 1.0 || v > self.h as f64 + 1.0 {
                    continue;
                }
                let s = self.sample_bilinear(u as f32, v as f32);
                row[x * 4..x * 4 + 4].copy_from_slice(&s);
            }
        });
        out
    }

    /// Half-size box downsample.
    pub fn downsample2(&self) -> Image {
        if self.w == 0 || self.h == 0 {
            return Image::new(0, 0);
        }
        let (w, h) = ((self.w / 2).max(1), (self.h / 2).max(1));
        let mut out = Image::new(w, h);
        out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let a = self.get_clamped(2 * x as isize, 2 * y as isize);
                let b = self.get_clamped(2 * x as isize + 1, 2 * y as isize);
                let c = self.get_clamped(2 * x as isize, 2 * y as isize + 1);
                let d = self.get_clamped(2 * x as isize + 1, 2 * y as isize + 1);
                for k in 0..4 {
                    row[x * 4 + k] = (a[k] + b[k] + c[k] + d[k]) * 0.25;
                }
            }
        });
        out
    }

    /// `self × (1 − w) + other × w` (same size; premultiplied, so this is a proper cross-fade).
    pub fn lerp(mut self, other: &Image, w: f32) -> Image {
        if other.w != self.w || other.h != self.h {
            return self;
        }
        for (a, b) in self.px.iter_mut().zip(&other.px) {
            *a += (*b - *a) * w;
        }
        self
    }

    pub fn scale_alpha(&mut self, a: f32) {
        if (a - 1.0).abs() < 1e-6 {
            return;
        }
        self.px.par_iter_mut().for_each(|v| *v *= a);
    }

    /// Straight-alpha colour of a pixel.
    #[inline]
    pub fn unpremul(p: [f32; 4]) -> [f32; 3] {
        if p[3] <= 1e-6 { [0.0; 3] } else { [p[0] / p[3], p[1] / p[3], p[2] / p[3]] }
    }

    /// Apply a per-pixel function on straight-alpha colour (alpha preserved).
    pub fn map_rgb(&mut self, f: impl Fn([f32; 3], usize, usize) -> [f32; 3] + Sync) {
        if self.w == 0 || self.h == 0 {
            return;
        }
        let w = self.w;
        self.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let p = &mut row[x * 4..x * 4 + 4];
                let a = p[3];
                if a <= 1e-6 {
                    continue;
                }
                let c = f([p[0] / a, p[1] / a, p[2] / a], x, y);
                p[0] = c[0] * a;
                p[1] = c[1] * a;
                p[2] = c[2] * a;
            }
        });
    }

    /// Convert to straight sRGB RGBA8.
    pub fn to_rgba8(&self) -> Vec<u8> {
        if self.w == 0 || self.h == 0 {
            return Vec::new();
        }
        let mut out = vec![0u8; self.w * self.h * 4];
        out.par_chunks_mut(self.w * 4).zip(self.px.par_chunks(self.w * 4)).for_each(|(o, s)| filmcraft_frame::linear_premul_to_srgb8(s, o));
        out
    }

    /// Opaque version over black (for monitors).
    pub fn over_black_rgba8(&self) -> Vec<u8> {
        if self.w == 0 || self.h == 0 {
            return Vec::new();
        }
        let mut out = vec![0u8; self.w * self.h * 4];
        out.par_chunks_mut(self.w * 4).zip(self.px.par_chunks(self.w * 4)).for_each(|(o, s)| over_black_row(s, o, false));
        out
    }

    /// [`Self::over_black_rgba8`] with each pixel's alpha in the fourth byte: the color bytes are
    /// the same (the sRGB of the premultiplied linear color), so the picture still shows over
    /// black, and the alpha says where it is transparent.
    pub fn over_black_with_alpha_rgba8(&self) -> Vec<u8> {
        if self.w == 0 || self.h == 0 {
            return Vec::new();
        }
        let mut out = vec![0u8; self.w * self.h * 4];
        out.par_chunks_mut(self.w * 4).zip(self.px.par_chunks(self.w * 4)).for_each(|(o, s)| over_black_row(s, o, true));
        out
    }
}

/// Pixels per batch of [`over_black_row`].
const BATCH: usize = 64;

/// One row of [`Image::over_black_rgba8`] (`alpha` false: the fourth byte is 255) or
/// [`Image::over_black_with_alpha_rgba8`]. A batch's table indices are computed first, in a loop
/// that vectorises, and looked up after; per value this is
/// [`filmcraft_color::linear_to_srgb_u8`], and [`alpha_u8`] for the alpha.
pub(crate) fn over_black_row(s: &[f32], o: &mut [u8], alpha: bool) {
    let lut = filmcraft_color::linear_to_srgb_u8_table();
    let mut ix = [0u16; BATCH * 4];
    for (s, o) in s.chunks(BATCH * 4).zip(o.chunks_mut(BATCH * 4)) {
        let ix = &mut ix[..s.len()];
        for (i, v) in ix.iter_mut().zip(s) {
            *i = filmcraft_color::linear_to_srgb_index(*v);
        }
        for ((o, i), s) in o.as_chunks_mut::<4>().0.iter_mut().zip(ix.as_chunks::<4>().0).zip(s.as_chunks::<4>().0) {
            o[0] = lut[i[0] as usize & 4095];
            o[1] = lut[i[1] as usize & 4095];
            o[2] = lut[i[2] as usize & 4095];
            o[3] = if alpha { alpha_u8(s[3]) } else { 255 };
        }
    }
}

/// `(a.clamp(0.0, 1.0) * 255.0).round() as u8` without the `round` library call: below 255 the
/// fraction is exact, so it rounds half away from zero the same way (NaN gives 0 both ways).
#[inline]
pub(crate) fn alpha_u8(a: f32) -> u8 {
    let x = a.clamp(0.0, 1.0) * 255.0;
    let i = x as i32;
    (i + i32::from(x - i as f32 >= 0.5)) as u8
}

/// One axis of an [`AxisResample`]: for each destination column (or row), the source cell
/// [`Image::sample_bilinear`] reads, its weight, and whether the destination pixel is skipped.
#[derive(Clone, Debug)]
struct AxisTaps {
    /// Destination range that can be covered, as [`Image::transformed`] bounds it.
    from: usize,
    to: usize,
    /// Per destination index in `from..to`: the left (top) source index, the weight of the next
    /// one, and whether the sample position is outside the source by more than one pixel.
    taps: Vec<(isize, f32, bool)>,
}

impl AxisTaps {
    /// `scale` and `offset` map a destination centre `d + 0.5` to its source coordinate, computed
    /// in f64 and narrowed to f32 exactly as [`Image::transformed_sampled`] and
    /// [`Image::sample_bilinear`] compute it.
    fn new(from: usize, to: usize, scale: f64, offset: f64, src_len: usize) -> Self {
        let taps = (from..to)
            .map(|d| {
                let u = scale * (d as f64 + 0.5) + offset;
                let skip = u < -1.0 || u > src_len as f64 + 1.0;
                let f = u as f32 - 0.5;
                let i = f.floor();
                (i as isize, f - i, skip)
            })
            .collect();
        AxisTaps { from, to, taps }
    }

    /// The source indices any tap that is not skipped reads, clipped to `0..src_len`.
    fn reach(&self, src_len: usize) -> (usize, usize) {
        let mut lo = isize::MAX;
        let mut hi = isize::MIN;
        for &(i, _, skip) in &self.taps {
            if !skip {
                lo = lo.min(i);
                hi = hi.max(i + 1);
            }
        }
        if lo > hi {
            return (0, 0);
        }
        let lo = lo.clamp(0, src_len as isize) as usize;
        let hi = (hi + 1).clamp(0, src_len as isize) as usize;
        (lo, hi.max(lo))
    }
}

/// [`Image::transformed`] for an axis-aligned placement that does not minify by 2 or more: the
/// same bilinear samples, with the source coordinate, cell and weight of each destination column and
/// row computed once rather than per pixel, and the four taps read without a bounds test where both
/// rows and both columns lie inside. It gives exactly the pixels of [`Image::transformed_sampled`],
/// and can read its source from a window of the full picture (see [`AxisResample::window`]).
#[derive(Clone, Debug)]
pub struct AxisResample {
    w: usize,
    h: usize,
    src_w: usize,
    src_h: usize,
    cols: AxisTaps,
    rows: AxisTaps,
}

impl AxisResample {
    /// `None` when either picture is empty, or the placement rotates or shears, is not invertible,
    /// or minifies by 2 or more.
    pub fn new(src_w: usize, src_h: usize, w: usize, h: usize, m: &Affine) -> Option<Self> {
        if src_w == 0 || src_h == 0 || w == 0 || h == 0 || m.b != 0.0 || m.c != 0.0 {
            return None;
        }
        let inv = m.inverse()?;
        let sx = (m.a * m.a + m.b * m.b).sqrt();
        let sy = (m.c * m.c + m.d * m.d).sqrt();
        let minify = 1.0 / sx.min(sy).max(1e-6);
        if minify >= 2.0 && src_w >= 4 && src_h >= 4 {
            return None;
        }
        let b = m.bounds(&filmcraft_geom::Rect::new(0.0, 0.0, src_w as f64, src_h as f64));
        let y0 = (b.y.floor().max(0.0) as usize).min(h);
        let y1 = (b.bottom().ceil().max(0.0) as usize).min(h);
        let x0 = (b.x.floor().max(0.0) as usize).min(w);
        let x1 = (b.right().ceil().max(0.0) as usize).min(w);
        let cols = AxisTaps::new(x0, x1.max(x0), inv.a, inv.e, src_w);
        let rows = AxisTaps::new(y0, y1.max(y0), inv.d, inv.f, src_h);
        Some(AxisResample { w, h, src_w, src_h, cols, rows })
    }

    /// The rectangle of the source the samples read: `(x, y, w, h)`. A picture holding only this
    /// window gives the same result through [`AxisResample::apply`] as the whole picture.
    pub fn window(&self) -> (usize, usize, usize, usize) {
        let (x0, x1) = self.cols.reach(self.src_w);
        let (y0, y1) = self.rows.reach(self.src_h);
        (x0, y0, x1 - x0, y1 - y0)
    }

    /// Resample `src`, which holds the source picture from `origin` on: the whole picture at
    /// `(0, 0)`, or at least [`AxisResample::window`].
    pub fn apply(&self, src: &Image, origin: (usize, usize)) -> Image {
        let mut out = Image::new(self.w, self.h);
        let (ox, oy) = (origin.0 as isize, origin.1 as isize);
        let (fw, fh) = (self.src_w as isize, self.src_h as isize);
        let (sw, sh) = (src.w as isize, src.h as isize);
        let fetch = |x: isize, y: isize| -> [f32; 4] {
            if x < 0 || y < 0 || x >= fw || y >= fh {
                return [0.0; 4];
            }
            let (lx, ly) = (x - ox, y - oy);
            if lx < 0 || ly < 0 || lx >= sw || ly >= sh {
                return [0.0; 4];
            }
            let i = (ly as usize * src.w + lx as usize) * 4;
            [src.px[i], src.px[i + 1], src.px[i + 2], src.px[i + 3]]
        };
        let cols = &self.cols;
        let rows = &self.rows;
        out.px.par_chunks_mut(self.w * 4).enumerate().skip(rows.from).take(rows.to - rows.from).for_each(|(y, row)| {
            let (y0, ty, yskip) = rows.taps[y - rows.from];
            if yskip {
                return;
            }
            // the two source rows, when both lie inside the source and the window
            let inside_rows = y0 >= 0 && y0 + 1 < fh && y0 - oy >= 0 && y0 + 1 - oy < sh;
            for (k, &(x0, tx, xskip)) in cols.taps.iter().enumerate() {
                if xskip {
                    continue;
                }
                let x = cols.from + k;
                let (a, b, c, d) = if inside_rows && x0 >= 0 && x0 + 1 < fw && x0 - ox >= 0 && x0 + 1 - ox < sw {
                    let i = ((y0 - oy) as usize * src.w + (x0 - ox) as usize) * 4;
                    let j = i + src.w * 4;
                    let p = &src.px;
                    (
                        [p[i], p[i + 1], p[i + 2], p[i + 3]],
                        [p[i + 4], p[i + 5], p[i + 6], p[i + 7]],
                        [p[j], p[j + 1], p[j + 2], p[j + 3]],
                        [p[j + 4], p[j + 5], p[j + 6], p[j + 7]],
                    )
                } else {
                    (fetch(x0, y0), fetch(x0 + 1, y0), fetch(x0, y0 + 1), fetch(x0 + 1, y0 + 1))
                };
                let o = &mut row[x * 4..x * 4 + 4];
                for ch in 0..4 {
                    let top = a[ch] + (b[ch] - a[ch]) * tx;
                    let bot = c[ch] + (d[ch] - c[ch]) * tx;
                    o[ch] = top + (bot - top) * ty;
                }
            }
        });
        out
    }
}

#[cfg(test)]
mod axis_tests {
    use super::*;

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    fn picture(w: usize, h: usize, rng: &mut Lcg) -> Image {
        let mut px = Vec::with_capacity(w * h * 4);
        for _ in 0..w * h {
            let a = rng.next() as f32;
            px.extend_from_slice(&[rng.next() as f32 * a, rng.next() as f32 * a, rng.next() as f32 * a, a]);
        }
        Image { w, h, px }
    }

    fn same_bits(a: &Image, b: &Image, what: &str) {
        assert_eq!((a.w, a.h), (b.w, b.h), "{what}: size");
        if let Some(i) = a.px.iter().zip(&b.px).position(|(x, y)| x.to_bits() != y.to_bits()) {
            panic!("{what}: first difference at pixel {} channel {}: {} vs {}", i / 4, i % 4, a.px[i], b.px[i]);
        }
    }

    fn window_of(src: &Image, (x, y, w, h): (usize, usize, usize, usize)) -> Image {
        let mut px = Vec::with_capacity(w * h * 4);
        for row in y..y + h {
            px.extend_from_slice(&src.px[(row * src.w + x) * 4..(row * src.w + x + w) * 4]);
        }
        Image { w, h, px }
    }

    fn agrees(src: &Image, w: usize, h: usize, m: &Affine, what: &str) {
        let reference = src.transformed_sampled(w, h, m);
        let Some(rs) = AxisResample::new(src.w, src.h, w, h, m) else { panic!("{what}: an axis-aligned placement") };
        same_bits(&rs.apply(src, (0, 0)), &reference, what);
        let (x, y, ww, wh) = rs.window();
        same_bits(&rs.apply(&window_of(src, (x, y, ww, wh)), (x, y)), &reference, &format!("{what}, from its window"));
        same_bits(&src.transformed(w, h, m), &reference, &format!("{what}, through transformed"));
    }

    #[test]
    fn a_landscape_picture_filled_into_a_portrait_frame_gives_the_general_paths_bits() {
        let mut rng = Lcg(7);
        let src = picture(64, 36, &mut rng);
        let scale = 64.0 / 36.0;
        let m = Affine::translate(-(64.0 * scale - 36.0) / 2.0, 0.0).then_apply(&Affine::scale(scale, scale));
        agrees(&src, 36, 64, &m, "the vertical fill");
        let (_, _, ww, _) = AxisResample::new(64, 36, 36, 64, &m).map(|rs| rs.window()).unwrap_or_default();
        assert!(ww < 30, "the fill reads the middle of the picture, not all 64 columns: {ww}");
    }

    #[test]
    fn random_scales_and_moves_give_the_general_paths_bits() {
        let mut rng = Lcg(42);
        for round in 0..60 {
            let (sw, sh) = (3 + (rng.next() * 40.0) as usize, 3 + (rng.next() * 40.0) as usize);
            let (w, h) = (1 + (rng.next() * 50.0) as usize, 1 + (rng.next() * 50.0) as usize);
            let src = picture(sw, sh, &mut rng);
            let (sx, sy) = (0.51 + rng.next() * 3.5, 0.51 + rng.next() * 3.5);
            let (tx, ty) = ((rng.next() - 0.5) * 60.0, (rng.next() - 0.5) * 60.0);
            let m = Affine::translate(tx, ty).then_apply(&Affine::scale(sx, sy));
            agrees(&src, w, h, &m, &format!("round {round}: {sw}x{sh} to {w}x{h} at {sx:.3}x{sy:.3} moved {tx:.2}, {ty:.2}"));
        }
    }

    #[test]
    fn a_rotation_a_shear_or_a_strong_minification_takes_the_general_path() {
        assert!(AxisResample::new(40, 30, 40, 30, &Affine::rotate_deg(17.0)).is_none());
        assert!(AxisResample::new(40, 30, 40, 30, &Affine { b: 0.2, ..Affine::scale(1.0, 1.0) }).is_none());
        assert!(AxisResample::new(40, 30, 10, 10, &Affine::scale(0.25, 0.25)).is_none());
        assert!(AxisResample::new(40, 30, 10, 10, &Affine::scale(0.0, 0.0)).is_none());
    }
}
