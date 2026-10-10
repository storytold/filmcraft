//! Text and vector graphics drawn with `filmcraft-text`: the Timecode / Clip Name burn-ins and
//! (see below) graphic clips' text and shape layers.

use filmcraft_color::srgb_to_linear;
use filmcraft_geom::Vec2;
use filmcraft_text::{Align, Mask, ParagraphStyle, TextStyle, layout, render};
use rayon::prelude::*;

use crate::image::Image;

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
    if text.is_empty() || pos.x.is_nan() {
        return;
    }
    let st = TextStyle { family: family.into(), style: "Regular".into(), size: px, ..Default::default() };
    let l = layout(text, &st, &ParagraphStyle { align: Align::Center, ..Default::default() });
    let vm = filmcraft_text::fonts::face(filmcraft_text::resolve(family, "Regular").face).metrics(px);
    // centre the cap height on pos
    let (bx, by) = (pos.x as f32, pos.y as f32 + vm.cap_height / 2.0);
    let pad = px * 0.25;
    let b = l.bounds;
    let boxp = filmcraft_text::Path::rect(bx + b[0] - pad, by - vm.cap_height - pad, bx + b[2] + pad, by + vm.descent * 0.6 + pad);
    if let Some(bb) = boxp.bounds() {
        let (x0, y0) = (bb.0.floor() as i32, bb.1.floor() as i32);
        let m =
            filmcraft_text::raster::fill(&boxp, (bb.2.ceil() as i32 - x0).max(1) as usize, (bb.3.ceil() as i32 - y0).max(1) as usize, -x0 as f32, -y0 as f32);
        paint_mask(img, &m, x0, y0, [0.0, 0.0, 0.0, 1.0], box_alpha);
    }
    let (m, x0, y0) = render::rasterize(&l, &render::at(bx, by.round()));
    paint_mask(img, &m, x0, y0, [1.0, 1.0, 1.0, 1.0], 1.0);
}
