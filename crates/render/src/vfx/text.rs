//! Text burn-ins: Simple Text and Metadata & Timecode Burn-in.

use filmcraft_project::EffectInstance;
use filmcraft_text::{Align, ParagraphStyle, TextStyle, layout, render};

use super::{FxCtx, Image, Vec2, chv, cv, fv, pv, tv};
use crate::graphics::paint_mask;

/// Where and how [`draw_text`] sets its text: font family, anchor, pixel size, alignment code
/// (0 left, 1 centre, 2 right), colour and opacity of the black box behind it.
#[derive(Clone, Copy)]
pub(crate) struct TextLook<'a> {
    pub family: &'a str,
    pub pos: Vec2,
    pub px: f32,
    pub align: u32,
    pub color: [f32; 4],
    pub box_alpha: f32,
}

/// Draw `text` with its first baseline's cap height centred on `pos` (layer pixels), aligned
/// left/centre/right on `pos.x`, over an optional black box.
pub(crate) fn draw_text(img: &mut Image, text: &str, look: TextLook<'_>) {
    let TextLook { family, pos, px, align, color, box_alpha } = look;
    if text.is_empty() || !pos.x.is_finite() || !pos.y.is_finite() {
        return;
    }
    let al = match align {
        0 => Align::Left,
        2 => Align::Right,
        _ => Align::Center,
    };
    let st = TextStyle { family: family.into(), style: "Regular".into(), size: px, ..Default::default() };
    let l = layout(text, &st, &ParagraphStyle { align: al, ..Default::default() });
    let vm = filmcraft_text::fonts::face(filmcraft_text::resolve(family, "Regular").face).metrics(px);
    let (bx, by) = (pos.x as f32, pos.y as f32 + vm.cap_height / 2.0);
    let b = l.bounds;
    if box_alpha > 0.0 {
        let pad = px * 0.25;
        let boxp = filmcraft_text::Path::rect(bx + b[0] - pad, by + b[1] - pad, bx + b[2] + pad, by + b[3] + pad);
        if let Some(bb) = boxp.bounds() {
            let (x0, y0) = (bb.0.floor() as i32, bb.1.floor() as i32);
            let m = filmcraft_text::raster::fill(
                &boxp,
                (bb.2.ceil() as i32 - x0).max(1) as usize,
                (bb.3.ceil() as i32 - y0).max(1) as usize,
                -x0 as f32,
                -y0 as f32,
            );
            paint_mask(img, &m, x0, y0, [0.0, 0.0, 0.0, 1.0], box_alpha.clamp(0.0, 1.0));
        }
    }
    let (m, x0, y0) = render::rasterize(&l, &render::at(bx, by.round()));
    paint_mask(img, &m, x0, y0, crate::graphics::premul_linear(color), 1.0);
}

/// Simple Text: a single block of text burned into the clip.
pub fn simple_text(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let text = tv(e, "text");
    let pos = pv(e, "position", cx, img);
    let size = (fv(e, "size", cx) * cx.px_scale).max(2.0);
    let family = ["Inter", "JetBrains Mono"][chv(e, "font").min(1) as usize];
    let color = cv(e, "color", cx);
    let bg = fv(e, "bg_opacity", cx) / 100.0;
    draw_text(img, &text, TextLook { family, pos, px: size, align: chv(e, "alignment"), color, box_alpha: bg });
}

/// The text a Metadata & Timecode Burn-in shows.
pub(crate) fn burnin_text(e: &EffectInstance, cx: &FxCtx) -> String {
    let body = match chv(e, "source") {
        0 => cx.timecode.to_string(),
        1 => cx.env.map_or_else(|| cx.timecode.to_string(), |env| env.media_timecode()),
        2 => cx.clip_name.to_string(),
        3 => cx.env.map_or_else(|| cx.clip_name.to_string(), |env| env.file_name()),
        4 => {
            let fps = cx.env.map_or(30.0, |env| env.frame_rate());
            format!("{}", (cx.seconds * fps).round() as i64)
        }
        _ => cx.env.map(|env| env.sequence_name()).unwrap_or_default(),
    };
    let prefix = tv(e, "prefix");
    if prefix.is_empty() { body } else { format!("{prefix} {body}") }
}

/// Metadata & Timecode Burn-in: sequence / media timecode, names or frame count in a box.
pub fn metadata_burnin(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let text = burnin_text(e, cx);
    let size = (fv(e, "size", cx) / 100.0 * img.h as f32).max(6.0);
    let (w, h) = (img.w as f64, img.h as f64);
    let margin = (size as f64) * 1.2;
    let (pos, align) = match chv(e, "alignment") {
        0 => (Vec2::new(w / 2.0, h - margin), 1),
        1 => (Vec2::new(margin, h - margin), 0),
        2 => (Vec2::new(w - margin, h - margin), 2),
        3 => (Vec2::new(w / 2.0, margin), 1),
        4 => (Vec2::new(margin, margin), 0),
        5 => (Vec2::new(w - margin, margin), 2),
        _ => (pv(e, "position", cx, img), 1),
    };
    let family = if matches!(chv(e, "source"), 0 | 1 | 4) { "JetBrains Mono" } else { "Inter" };
    draw_text(img, &text, TextLook { family, pos, px: size, align, color: cv(e, "color", cx), box_alpha: fv(e, "opacity", cx) / 100.0 });
}
