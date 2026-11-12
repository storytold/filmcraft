//! Text and vector graphics drawn with `filmcraft-text`: the Timecode / Clip Name burn-ins and
//! (see below) graphic clips' text and shape layers.

use std::sync::Arc;

use filmcraft_color::srgb_to_linear;
use filmcraft_geom::Vec2;
use filmcraft_text::{Align, Layout, Mask, ParagraphStyle, Path, TextStyle, Xform, layout, render};
use rayon::prelude::*;

use crate::graphic_clip::LayerRaster;
use crate::image::Image;

/// Effect text is metadata-controlled; do not let its measured bounds allocate an unbounded mask.
const MAX_BURN_TEXT_PIXELS: usize = 16 << 20;

pub(crate) fn bounded_raster_size(w: f32, h: f32) -> Option<(usize, usize)> {
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let (w, h) = (w.ceil() as usize, h.ceil() as usize);
    (w.checked_mul(h)? <= MAX_BURN_TEXT_PIXELS).then_some((w, h))
}

/// sRGB-encoded straight colour (0..1) → premultiplied linear.
pub fn premul_linear(c: [f32; 4]) -> [f32; 4] {
    let a = c[3].clamp(0.0, 1.0);
    [srgb_to_linear(c[0]) * a, srgb_to_linear(c[1]) * a, srgb_to_linear(c[2]) * a, a]
}

/// Blend two sRGB straight colours in linear light, then premultiply. `t` is 0 at `start`.
pub fn premul_linear_lerp(start: [f32; 4], end: [f32; 4], t: f32) -> [f32; 4] {
    let t = t.clamp(0.0, 1.0);
    let s = [srgb_to_linear(start[0]), srgb_to_linear(start[1]), srgb_to_linear(start[2]), start[3].clamp(0.0, 1.0)];
    let e = [srgb_to_linear(end[0]), srgb_to_linear(end[1]), srgb_to_linear(end[2]), end[3].clamp(0.0, 1.0)];
    let a = s[3] * (1.0 - t) + e[3] * t;
    [(s[0] * (1.0 - t) + e[0] * t) * a, (s[1] * (1.0 - t) + e[1] * t) * a, (s[2] * (1.0 - t) + e[2] * t) * a, a]
}

/// Composite `color` (premultiplied linear) through coverage `m` placed at (`x0`, `y0`), scaled by
/// `opacity`, over `img`.
pub fn paint_mask(img: &mut Image, m: &Mask, x0: i32, y0: i32, color: [f32; 4], opacity: f32) {
    if m.w == 0 || m.h == 0 || color[3] <= 0.0 || opacity <= 0.0 {
        return;
    }
    let w = img.w;
    let ys = y0.max(0) as usize..((y0 + m.h as i32).min(img.h as i32)).max(0) as usize;
    if ys.is_empty() {
        return;
    }
    let xs = x0.max(0)..(x0 + m.w as i32).min(w as i32);
    if xs.is_empty() {
        return;
    }
    let rows = &mut img.px[ys.start * w * 4..ys.end * w * 4];
    rows.par_chunks_mut(w * 4).enumerate().for_each(|(ry, row)| {
        let my = (ys.start + ry) as i32 - y0;
        let mrow = &m.a[my as usize * m.w..(my as usize + 1) * m.w];
        for x in xs.clone() {
            let cov = mrow[(x - x0) as usize] * opacity;
            if cov <= 0.0 {
                continue;
            }
            let p = &mut row[x as usize * 4..x as usize * 4 + 4];
            let k = 1.0 - color[3] * cov;
            for c in 0..4 {
                p[c] = color[c] * cov + p[c] * k;
            }
        }
    });
}

/// Draw `text` centred on `pos` (layer pixels) at `px` pixels in white over a black box of
/// `box_alpha` (the Timecode and Clip Name effects).
pub fn burn_text(img: &mut Image, text: &str, family: &str, pos: Vec2, px: f32, box_alpha: f32) {
    if let Some(burn) = BurnText::new(text, family, pos, px) {
        burn.paint(img, (0, 0), box_alpha);
    }
}

/// Rasterise the visible part of a burn-in for compositing the text and box at one opacity.
pub fn burn_text_layer(canvas: (usize, usize), text: &str, family: &str, pos: Vec2, px: f32, box_alpha: f32) -> Option<LayerRaster> {
    let burn = BurnText::new(text, family, pos, px)?;
    let (left, top, right, bottom) = burn.box_path.bounds()?;
    // rasterize() includes a one-pixel fringe around the glyph bounds. Multiline text
    // and overhanging glyphs can both extend beyond the backing box.
    let [gx0, gy0, gx1, gy1] = render::device_bounds(&burn.layout, &burn.transform);
    let x = (left.floor().min(gx0.floor() - 1.0).max(0.0) as usize).min(canvas.0);
    let y = (top.floor().min(gy0.floor() - 1.0).max(0.0) as usize).min(canvas.1);
    let end_x = (right.ceil().max(gx1.ceil() + 1.0).max(0.0) as usize).min(canvas.0);
    let end_y = (bottom.ceil().max(gy1.ceil() + 1.0).max(0.0) as usize).min(canvas.1);
    if x >= end_x || y >= end_y {
        return None;
    }
    let mut img = Image::new(end_x - x, end_y - y);
    let (x, y) = (i32::try_from(x).ok()?, i32::try_from(y).ok()?);
    burn.paint(&mut img, (x, y), box_alpha);
    Some(LayerRaster { x, y, img })
}

struct BurnText {
    layout: Arc<Layout>,
    transform: Xform,
    box_path: Path,
}

impl BurnText {
    fn new(text: &str, family: &str, pos: Vec2, px: f32) -> Option<Self> {
        if text.is_empty() || !pos.x.is_finite() || !pos.y.is_finite() || !px.is_finite() || px <= 0.0 {
            return None;
        }
        let st = TextStyle { family: family.into(), style: "Regular".into(), size: px, ..Default::default() };
        let layout = layout(text, &st, &ParagraphStyle { align: Align::Center, ..Default::default() });
        let vm = filmcraft_text::fonts::face(filmcraft_text::resolve(family, "Regular").face).metrics(px);
        // centre the cap height on pos
        let (bx, by) = (pos.x as f32, pos.y as f32 + vm.cap_height / 2.0);
        let pad = px * 0.25;
        let b = layout.bounds;
        let box_path = Path::rect(bx + b[0] - pad, by - vm.cap_height - pad, bx + b[2] + pad, by + vm.descent * 0.6 + pad);
        Some(Self { layout, transform: render::at(bx, by.round()), box_path })
    }

    fn paint(&self, img: &mut Image, origin: (i32, i32), box_alpha: f32) {
        if let Some(bb) = self.box_path.bounds() {
            let (x0, y0) = (bb.0.floor() as i32, bb.1.floor() as i32);
            if let Some((w, h)) = bounded_raster_size(bb.2 - x0 as f32, bb.3 - y0 as f32) {
                let m = filmcraft_text::raster::fill(&self.box_path, w, h, -(x0 as f32), -(y0 as f32));
                paint_mask(img, &m, x0.saturating_sub(origin.0), y0.saturating_sub(origin.1), [0.0, 0.0, 0.0, 1.0], box_alpha);
            }
        }
        let bounds = self.layout.bounds;
        if bounded_raster_size(bounds[2] - bounds[0], bounds[3] - bounds[1]).is_none() {
            return;
        }
        let (m, x0, y0) = render::rasterize(&self.layout, &self.transform);
        paint_mask(img, &m, x0.saturating_sub(origin.0), y0.saturating_sub(origin.1), [1.0; 4], 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burn_in_layer_only_allocates_the_visible_text_region() {
        let layer_at = |pos| burn_text_layer((3840, 2160), "01:02:03:12", filmcraft_text::fonts::DEFAULT_FAMILY, pos, 24.0, 0.6);
        let layer = layer_at(Vec2::new(1920.25, 1080.5)).unwrap();
        assert!(layer.x > 0 && layer.y > 0);
        assert!((1..512).contains(&layer.img.w) && (1..128).contains(&layer.img.h));
        let clipped = layer_at(Vec2::new(0.25, 0.5)).unwrap();
        assert_eq!((clipped.x, clipped.y), (0, 0));
        assert!(clipped.img.w < layer.img.w && clipped.img.h < layer.img.h);
        assert!(layer_at(Vec2::new(-1000.0, 0.0)).is_none());
    }

    #[test]
    fn burn_in_raster_budget_rejects_huge_and_non_finite_bounds() {
        assert_eq!(bounded_raster_size(4096.0, 4096.0), Some((4096, 4096)));
        assert_eq!(bounded_raster_size(4097.0, 4096.0), None);
        assert_eq!(bounded_raster_size(f32::INFINITY, 1.0), None);
    }
}
