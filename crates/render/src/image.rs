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
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, px: vec![0.0; w * h * 4] }
    }
    pub fn filled(w: usize, h: usize, c: [f32; 4]) -> Self {
        let mut px = Vec::with_capacity(w * h * 4);
        for _ in 0..w * h {
            px.extend_from_slice(&c);
        }
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
    pub fn transformed(&self, w: usize, h: usize, m: &Affine) -> Image {
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
        let mut out = vec![0u8; self.w * self.h * 4];
        out.par_chunks_mut(self.w * 4).zip(self.px.par_chunks(self.w * 4)).for_each(|(o, s)| filmcraft_frame::linear_premul_to_srgb8(s, o));
        out
    }

    /// Opaque version over black (for monitors).
    pub fn over_black_rgba8(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.w * self.h * 4];
        out.par_chunks_mut(self.w * 4).zip(self.px.par_chunks(self.w * 4)).for_each(|(o, s)| {
            for (o, s) in o.as_chunks_mut::<4>().0.iter_mut().zip(s.as_chunks::<4>().0) {
                o[0] = filmcraft_color::linear_to_srgb_u8(s[0]);
                o[1] = filmcraft_color::linear_to_srgb_u8(s[1]);
                o[2] = filmcraft_color::linear_to_srgb_u8(s[2]);
                o[3] = 255;
            }
        });
        out
    }
}
