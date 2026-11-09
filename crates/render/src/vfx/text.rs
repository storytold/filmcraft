//! Text burn-ins: Simple Text and Metadata & Timecode Burn-in.

use filmcraft_project::EffectInstance;
use filmcraft_text::{Align, ParagraphStyle, TextStyle, layout, render};
use filmcraft_time::TimeDisplay;

use super::{FxCtx, Image, Vec2, bv, chv, cv, fbm, fv, pv, smoothstep, tv};
use crate::graphics::paint_mask;
use rayon::prelude::*;

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

/// The burn-in's Time Format choices, in the order of the effect's `time_format` list.
const TIME_FORMATS: [TimeDisplay; 4] = [TimeDisplay::Timecode, TimeDisplay::Feet35, TimeDisplay::Feet16, TimeDisplay::Frames];

/// The film time format (not Timecode) of a burn-in's timecode sources, which the host formats.
pub(crate) fn film_time_format(e: &EffectInstance) -> Option<TimeDisplay> {
    let display = TIME_FORMATS[(chv(e, "time_format") as usize).min(TIME_FORMATS.len() - 1)];
    (e.effect == "metadata_burnin" && display != TimeDisplay::Timecode && matches!(chv(e, "source"), 0 | 1)).then_some(display)
}

/// The text a Metadata & Timecode Burn-in shows.
pub(crate) fn burnin_text(e: &EffectInstance, cx: &FxCtx) -> String {
    // Time Format applies to the two timecode sources; Timecode keeps the precomputed string
    // (drop-frame aware), the film formats are formatted by the host
    let display = TIME_FORMATS[(chv(e, "time_format") as usize).min(TIME_FORMATS.len() - 1)];
    let body = match chv(e, "source") {
        0 if display != TimeDisplay::Timecode => cx.env.and_then(|env| env.sequence_time(display)).unwrap_or_else(|| cx.timecode.to_string()),
        0 => cx.timecode.to_string(),
        1 if display != TimeDisplay::Timecode => {
            cx.env.and_then(|env| env.media_time(display)).unwrap_or_else(|| cx.env.map_or_else(|| cx.timecode.to_string(), |env| env.media_timecode()))
        }
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

/// Burning Text: dynamic perlin noise burning effect on customizable text.
pub fn burning_text(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let text = tv(e, "text");
    if text.is_empty() {
        return;
    }
    let font_idx = chv(e, "font");
    let family = match font_idx {
        1 => "JetBrains Mono",
        2 => "Noto Serif",
        3 => "Stalinist One",
        _ => "Inter",
    };
    let px = (fv(e, "size", cx) * cx.px_scale).clamp(2.0, 1000.0);
    let preset = chv(e, "position_preset");
    let progress = (fv(e, "progress", cx) / 100.0).clamp(0.0, 1.0);
    let direction = chv(e, "direction");
    let orange_glow = bv(e, "orange_glow");
    let glow_color = cv(e, "glow_color", cx);
    let text_color = cv(e, "color", cx);
    let noise_scale = (fv(e, "noise_scale", cx) * cx.px_scale).clamp(1.0, 1000.0);
    let noise_speed = fv(e, "noise_speed", cx).clamp(0.0, 100.0);

    let st = TextStyle { family: family.into(), style: "Regular".into(), size: px, ..Default::default() };
    let l = layout(&text, &st, &ParagraphStyle { align: Align::Center, ..Default::default() });
    let vm = filmcraft_text::fonts::face(filmcraft_text::resolve(family, "Regular").face).metrics(px);

    let (w, h) = (img.w as f64, img.h as f64);
    let margin = (px as f64) * 1.2;
    let pos_override = pv(e, "position", cx, img);
    let has_explicit_pos = e.param("position").is_some_and(|p| {
        let v = p.vec2_at(cx.t);
        v.x.is_finite() && v.y.is_finite()
    });
    let pos = if has_explicit_pos {
        pos_override
    } else {
        match preset {
            1 => Vec2::new(w / 2.0, margin + vm.cap_height as f64 / 2.0),
            2 => Vec2::new(w / 2.0, h - margin - vm.cap_height as f64 / 2.0),
            3 => Vec2::new(margin + (l.bounds[2] - l.bounds[0]) as f64 / 2.0, h / 2.0),
            4 => Vec2::new(w - margin - (l.bounds[2] - l.bounds[0]) as f64 / 2.0, h / 2.0),
            _ => Vec2::new(w / 2.0, h / 2.0),
        }
    };

    if !pos.x.is_finite() || !pos.y.is_finite() {
        return;
    }
    // Discard text positioned completely out of any reasonable frame boundaries to avoid coordinate overflow
    let bound_limit = 100_000.0;
    if pos.x < -bound_limit || pos.x > bound_limit || pos.y < -bound_limit || pos.y > bound_limit {
        return;
    }

    let (bx, by) = (pos.x as f32, pos.y as f32 + vm.cap_height / 2.0);
    let (m, x0, y0) = render::rasterize(&l, &render::at(bx, by.round()));
    if m.w == 0 || m.h == 0 {
        return;
    }

    if progress >= 1.0 && !orange_glow {
        return;
    }

    let col_premul = crate::graphics::premul_linear(text_color);
    let glow_lin = crate::graphics::premul_linear(glow_color);

    let img_w = img.w;
    let mh_i32 = m.h.min(i32::MAX as usize) as i32;
    let mw_i32 = m.w.min(i32::MAX as usize) as i32;
    let imgh_i32 = img.h.min(i32::MAX as usize) as i32;
    let imgw_i32 = img_w.min(i32::MAX as usize) as i32;
    let ys = y0.max(0) as usize..(y0.saturating_add(mh_i32).min(imgh_i32)).max(0) as usize;
    if ys.is_empty() {
        return;
    }
    let xs = x0.max(0)..x0.saturating_add(mw_i32).min(imgw_i32);
    if xs.is_empty() {
        return;
    }

    let soft: f32 = 0.08;
    let thr = -soft + progress * (1.0 + 2.0 * soft);
    let time_z = (cx.seconds as f32) * noise_speed;
    let seed: u64 = 0x57a1_101e;
    let mw_f = m.w.max(1) as f32;
    let mh_f = m.h.max(1) as f32;

    let rows = &mut img.px[ys.start * img_w * 4..ys.end * img_w * 4];
    rows.par_chunks_mut(img_w * 4).enumerate().for_each(|(ry, row)| {
        let y_img = (ys.start + ry) as i32;
        let my = y_img.saturating_sub(y0) as usize;
        if my >= m.h {
            return;
        }
        let mrow = &m.a[my * m.w..(my + 1) * m.w];
        let v = (my as f32 / mh_f).clamp(0.0, 1.0);

        for x_img in xs.clone() {
            let mx = x_img.saturating_sub(x0) as usize;
            if mx >= m.w {
                continue;
            }
            let cov = mrow[mx];
            if cov <= 0.0 {
                continue;
            }
            let u = (mx as f32 / mw_f).clamp(0.0, 1.0);

            let sweep = match direction {
                1 => v,
                2 => 1.0 - u,
                3 => u,
                _ => 1.0 - v,
            };

            let nx = x_img as f32 / noise_scale;
            let ny = y_img as f32 / noise_scale;
            let n = fbm(nx, ny, time_z, 3.0, seed);
            let noise_val = (n * 0.5 + 0.5).clamp(0.0, 1.0);

            let burn_val = 0.65 * sweep + 0.35 * noise_val;
            let d = burn_val - thr;

            let vis = smoothstep(-soft * 0.5, soft * 0.5, d);
            if vis <= 0.0 && !orange_glow {
                continue;
            }

            let char_factor = if d > 0.0 { 1.0 - 0.7 * (-(d / soft).powi(2)).exp() } else { 0.3 };

            let text_alpha = cov * vis * col_premul[3];
            let p = &mut row[x_img as usize * 4..x_img as usize * 4 + 4];

            if text_alpha > 0.0 {
                let k = 1.0 - text_alpha;
                for c in 0..3 {
                    p[c] = col_premul[c] * char_factor * cov * vis + p[c] * k;
                }
                p[3] = text_alpha + p[3] * k;
            }

            if orange_glow {
                let edge = (-(d / (soft * 0.7)).powi(2)).exp();
                let glow_strength = edge * 2.5 * cov;
                if glow_strength > 0.001 {
                    for c in 0..3 {
                        p[c] += glow_lin[c] * glow_strength;
                    }
                    p[3] = (p[3] + (1.0 - p[3]) * (glow_strength * 0.5).min(1.0)).clamp(0.0, 1.0);
                }
            }
        }
    });
}
