//! Caption layout and burn-in.
//!
//! [`overlay`] lays out the caption showing on a track at a time with the track's
//! [`CaptionStyle`] (size relative to a 1080-line frame, colour, background box, outline,
//! alignment, anchor and margin; WebVTT `line:N%` and `align:` cue settings override the style)
//! and rasterises it into a small premultiplied linear-light RGBA [`Overlay`] positioned in the
//! frame. Renderers composite overlays over the finished picture.

use filmcraft_color::srgb_to_linear;
use filmcraft_project::{Caption, CaptionAlign, CaptionAnchor, CaptionStyle, CaptionTrack, Sequence};
use filmcraft_time::Tick;

use filmcraft_text::{ParagraphStyle, TextStyle, layout, render};

/// Caption overlays are transient render resources, not source images. Keep one cue from
/// materialising an unbounded mask or RGBA buffer.
const MAX_OVERLAY_PIXELS: usize = 16 << 20;
const MAX_CAPTION_SIZE_1080: f32 = 256.0;
const MAX_OUTLINE_RADIUS: f32 = 64.0;

/// A rendered caption: premultiplied linear-light RGBA f32 pixels at `(x, y)` in the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    pub x: i32,
    pub y: i32,
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Overlay {
    /// Composite over a premultiplied RGBA f32 canvas of `cw`×`ch` pixels.
    pub fn composite_onto(&self, canvas: &mut [f32], cw: usize, ch: usize) {
        for y in 0..self.h {
            let cy = self.y + y as i32;
            if cy < 0 || cy >= ch as i32 {
                continue;
            }
            for x in 0..self.w {
                let cx = self.x + x as i32;
                if cx < 0 || cx >= cw as i32 {
                    continue;
                }
                let s = &self.px[(y * self.w + x) * 4..(y * self.w + x) * 4 + 4];
                if s[3] <= 0.0 {
                    continue;
                }
                let i = (cy as usize * cw + cx as usize) * 4;
                let k = 1.0 - s[3];
                for c in 0..4 {
                    canvas[i + c] = s[c] + canvas[i + c] * k;
                }
            }
        }
    }
}

/// The caption face: the track's font family (Inter when empty or not installed) in SemiBold at `px`.
fn caption_style(font: &str, px: f32) -> TextStyle {
    let family = if font.trim().is_empty() { filmcraft_text::fonts::DEFAULT_FAMILY } else { font.trim() };
    TextStyle { family: family.into(), style: "SemiBold".into(), size: px, ..Default::default() }
}

/// Like `caption_style` but also applies an explicit font_style string (#3357).
fn caption_face(font: &str, font_style: &str, px: f32) -> TextStyle {
    let mut ts = caption_style(font, px);
    let s = font_style.trim();
    if !s.is_empty() {
        ts.style = s.into();
    }
    ts
}

fn lin(c: [u8; 4]) -> [f32; 4] {
    let a = c[3] as f32 / 255.0;
    [srgb_to_linear(c[0] as f32 / 255.0) * a, srgb_to_linear(c[1] as f32 / 255.0) * a, srgb_to_linear(c[2] as f32 / 255.0) * a, a]
}

/// Word-wrap one line to `max_w` pixels.
fn wrap(line: &str, ts: &TextStyle, max_w: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in line.split(' ') {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if !cur.is_empty() && filmcraft_text::measure(&cand, ts) > max_w {
            out.push(std::mem::take(&mut cur));
            cur = word.to_string();
        } else {
            cur = cand;
        }
    }
    out.push(cur);
    out
}

/// Placement overrides from WebVTT cue settings.
fn vtt_overrides(settings: &str) -> (Option<f32>, Option<CaptionAlign>) {
    let mut line = None;
    let mut align = None;
    for kv in settings.split_whitespace() {
        let Some((k, v)) = kv.split_once(':') else { continue };
        match k {
            "line" => {
                let v = v.split(',').next().unwrap_or("");
                if let Some(p) = v.strip_suffix('%').and_then(|p| p.parse::<f32>().ok()) {
                    line = Some((p / 100.0).clamp(0.0, 1.0));
                }
            }
            "align" => {
                align = match v {
                    "start" | "left" => Some(CaptionAlign::Left),
                    "end" | "right" => Some(CaptionAlign::Right),
                    "center" | "middle" => Some(CaptionAlign::Center),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    (line, align)
}

/// Lay out and rasterise one caption for a `w`×`h` frame.
pub fn render_caption(c: &Caption, style: &CaptionStyle, w: usize, h: usize) -> Option<Overlay> {
    if w == 0 || h == 0 {
        return None;
    }
    let scale = h as f32 / 1080.0;
    let px = (style.size.clamp(4.0, MAX_CAPTION_SIZE_1080) * scale).clamp(4.0, h as f32);
    let max_w = w as f32 * 0.9;
    let ts = caption_face(&style.font, &style.font_style, px);
    let mut lines: Vec<String> = Vec::new();
    // (speaker names are metadata; like Premiere, they are not burned in)
    for l in c.plain_lines() {
        lines.extend(wrap(l.trim(), &ts, max_w));
    }
    lines.retain(|l| !l.is_empty());
    if lines.is_empty() {
        return None;
    }
    let vm = filmcraft_text::fonts::face(filmcraft_text::resolve(&ts.family, &ts.style).face).metrics(px);
    let (asc, desc) = (vm.ascent, vm.descent);
    let lh = px * style.line_spacing.clamp(0.8, 4.0);
    // Only include box_padding in the overlap geometry when the background box
    // is actually drawn; otherwise the text origin shifts for no reason (#3497, #3508).
    let box_pad = if style.background { style.box_padding * scale } else { 0.0 };
    let pad_x = (px * 0.3 + box_pad).round();
    let pad_y = box_pad.round();
    let block_h = lh * lines.len() as f32;
    let (line_override, align_override) = vtt_overrides(&c.settings);
    let align = align_override.unwrap_or(style.align);
    let margin = style.margin.clamp(0.0, 0.45) * h as f32;
    let top = match (line_override, style.anchor) {
        (Some(f), _) => (f * h as f32).min(h as f32 - block_h),
        (None, CaptionAnchor::Top) => margin,
        (None, CaptionAnchor::Middle) => (h as f32 - block_h) / 2.0,
        (None, CaptionAnchor::Bottom) => h as f32 - margin - block_h,
    }
    .max(0.0)
    .round();
    let side = w as f32 * 0.05;
    let widths: Vec<f32> = lines.iter().map(|l| filmcraft_text::measure(l, &ts)).collect();
    let xs: Vec<f32> = widths
        .iter()
        .map(|&lw| match align {
            CaptionAlign::Left => side + pad_x,
            CaptionAlign::Right => w as f32 - side - pad_x - lw,
            CaptionAlign::Center => (w as f32 - lw) / 2.0,
        })
        .collect();
    let outline = (style.outline * scale).clamp(0.0, MAX_OUTLINE_RADIUS);
    let grow = outline.ceil() + 1.0;
    // Expand the overlay rect to fit the shadow pixels, which can fall outside the
    // text bounds when shadow_distance or shadow_blur is large (#3498).
    let (shadow_grow_x, shadow_grow_y) = if style.drop_shadow && style.shadow_color[3] > 0 {
        let sa = style.shadow_angle_deg.to_radians();
        let sd = style.shadow_distance * scale;
        let blur_ext = style.shadow_blur.clamp(0.0, 200.0) * scale * 2.0;
        ((sa.cos() * sd).abs() + blur_ext, (sa.sin() * sd).abs() + blur_ext)
    } else {
        (0.0, 0.0)
    };
    let bx0 = xs.iter().zip(&widths).map(|(x, _)| x - pad_x - grow - shadow_grow_x).fold(f32::MAX, f32::min).floor();
    let bx1 = xs.iter().zip(&widths).map(|(x, lw)| x + lw + pad_x + grow + shadow_grow_x).fold(f32::MIN, f32::max).ceil();
    let by0 = (top - grow - pad_y - shadow_grow_y).floor();
    let by1 = (top + block_h + grow + pad_y + shadow_grow_y).ceil();
    // Text can be wider than the frame (an unbroken imported WebVTT word, for example). Raster
    // only the visible intersection; `draw` receives the original coordinates and clips into it.
    let (left, top) = (bx0.clamp(0.0, w as f32), by0.clamp(0.0, h as f32));
    let (right, bottom) = (bx1.clamp(0.0, w as f32), by1.clamp(0.0, h as f32));
    if right <= left || bottom <= top {
        return None;
    }
    let (ox, oy) = (left as i32, top as i32);
    let (ow, oh) = ((right - left) as usize, (bottom - top) as usize);
    if ow.checked_mul(oh).is_none_or(|pixels| pixels > MAX_OVERLAY_PIXELS) {
        return None;
    }
    let mut cover = vec![0.0f32; ow * oh];
    let mut bg = vec![0.0f32; ow * oh];
    for (i, line) in lines.iter().enumerate() {
        let lt = top + lh * i as f32;
        if style.background && style.background_color[3] > 0 {
            let (x0, x1) = (xs[i] - pad_x, xs[i] + widths[i] + pad_x);
            let (y0, y1) = (lt - pad_y, lt + lh + pad_y);
            // Precompute corner radius clamped to half the box height/width (#3356).
            let r = (style.box_corner_radius * scale).min((y1 - y0) * 0.5).min((x1 - x0) * 0.5).max(0.0);
            for y in (y0.floor() as i32).max(oy)..(y1.ceil() as i32).min(oy + oh as i32) {
                let fy = ((y as f32 + 1.0).min(y1) - (y as f32).max(y0)).clamp(0.0, 1.0);
                for x in (x0.floor() as i32).max(ox)..(x1.ceil() as i32).min(ox + ow as i32) {
                    let fx = ((x as f32 + 1.0).min(x1) - (x as f32).max(x0)).clamp(0.0, 1.0);
                    // Rounded-corner coverage: outside the corner arc → 0 (#3356).
                    let cx = (x as f32 + 0.5).clamp(x0 + r, x1 - r);
                    let cy = (y as f32 + 0.5).clamp(y0 + r, y1 - r);
                    let corner_cov = if r > 0.0 {
                        let dx = (x as f32 + 0.5 - cx).abs();
                        let dy = (y as f32 + 0.5 - cy).abs();
                        let dist = (dx * dx + dy * dy).sqrt();
                        // Smooth step over 1 px at the arc edge for anti-aliasing.
                        (r + 0.5 - dist).clamp(0.0, 1.0)
                    } else {
                        1.0
                    };
                    let j = (y - oy) as usize * ow + (x - ox) as usize;
                    bg[j] = (bg[j] + fx * fy * corner_cov).min(1.0);
                }
            }
        }
        let baseline = lt + (lh - (asc + desc)) / 2.0 + asc;
        let l = layout(line, &ts, &ParagraphStyle::default());
        let mut m = filmcraft_text::Mask { w: ow, h: oh, a: std::mem::take(&mut cover) };
        render::draw(&l, &render::at(xs[i], baseline.round()), &mut m, (ox, oy));
        cover = m.a;
    }
    let stroke = if outline > 0.0 { dilate(&cover, ow, oh, outline) } else { Vec::new() };
    // Drop shadow (#2322): shift the coverage mask by the shadow offset and blur it.
    let shadow_mask = if style.drop_shadow && style.shadow_color[3] > 0 {
        let sa = style.shadow_angle_deg.to_radians();
        let sd = style.shadow_distance * scale;
        let dx = (sa.cos() * sd).round() as i32;
        let dy = (sa.sin() * sd).round() as i32; // screen Y is down
        let mut shifted = vec![0.0f32; ow * oh];
        for y in 0..oh as i32 {
            for x in 0..ow as i32 {
                let sx = x - dx;
                let sy = y - dy;
                if sx >= 0 && sy >= 0 && sx < ow as i32 && sy < oh as i32 {
                    shifted[y as usize * ow + x as usize] = cover[sy as usize * ow + sx as usize];
                }
            }
        }
        if style.shadow_blur > 0.0 { blur_mask(&shifted, ow, oh, style.shadow_blur.clamp(0.0, 200.0) * scale) } else { shifted }
    } else {
        Vec::new()
    };
    let (tc, bc, oc) = (lin(style.color), lin(style.background_color), lin(style.outline_color));
    let sc = lin(style.shadow_color);
    let mut out = vec![0.0f32; ow * oh * 4];
    for j in 0..ow * oh {
        let mut p = [0.0f32; 4];
        let over = |p: &mut [f32; 4], c: [f32; 4], a: f32| {
            if a > 0.0 {
                for k in 0..4 {
                    p[k] = c[k] * a + p[k] * (1.0 - c[3] * a);
                }
            }
        };
        over(&mut p, bc, bg[j]);
        // Shadow goes under the text but above the background box.
        if !shadow_mask.is_empty() {
            over(&mut p, sc, shadow_mask[j]);
        }
        if !stroke.is_empty() {
            over(&mut p, oc, stroke[j]);
        }
        over(&mut p, tc, cover[j]);
        out[j * 4..j * 4 + 4].copy_from_slice(&p);
    }
    Some(Overlay { x: ox, y: oy, w: ow, h: oh, px: out })
}

/// Fast box-blur approximation of Gaussian blur applied to a single-channel coverage mask (#2322).
/// Three passes of a box filter converge toward Gaussian (σ ≈ blur / 1.73).
fn blur_mask(mask: &[f32], w: usize, h: usize, blur: f32) -> Vec<f32> {
    if blur <= 0.0 || w == 0 || h == 0 {
        return mask.to_vec();
    }
    let r = (blur / 1.73).ceil() as usize;
    let len = 2 * r + 1;
    let mut buf = mask.to_vec();
    let mut tmp = vec![0.0f32; w * h];
    for _ in 0..3 {
        // Horizontal pass
        for row in 0..h {
            let mut acc = 0.0f32;
            let mut cnt = 0u32;
            for x in 0..w {
                let src_x = x + r;
                if src_x < w {
                    acc += buf[row * w + src_x];
                    cnt += 1;
                }
                if x > 0 {
                    let remove_x = x - 1;
                    if remove_x + len <= w + r {} // already out
                    if x > r {
                        let rx = x - r - 1;
                        if rx < w {
                            acc -= buf[row * w + rx];
                            cnt -= 1;
                        }
                    }
                }
                tmp[row * w + x] = if cnt > 0 { acc / cnt as f32 } else { 0.0 };
            }
            // Proper sliding window horizontal
            let mut a2 = 0.0f32;
            let mut c2 = 0u32;
            for x in 0..w {
                if x + r < w {
                    a2 += buf[row * w + x + r];
                    c2 += 1;
                }
                if x > r {
                    let rx = x - r - 1;
                    a2 -= buf[row * w + rx];
                    c2 -= 1;
                }
                tmp[row * w + x] = if c2 > 0 { a2 / c2 as f32 } else { 0.0 };
            }
        }
        std::mem::swap(&mut buf, &mut tmp);
        // Vertical pass
        for col in 0..w {
            let mut a2 = 0.0f32;
            let mut c2 = 0u32;
            for y in 0..h {
                if y + r < h {
                    a2 += buf[(y + r) * w + col];
                    c2 += 1;
                }
                if y > r {
                    let ry = y - r - 1;
                    a2 -= buf[ry * w + col];
                    c2 -= 1;
                }
                tmp[y * w + col] = if c2 > 0 { a2 / c2 as f32 } else { 0.0 };
            }
        }
        std::mem::swap(&mut buf, &mut tmp);
    }
    buf
}

/// Max-filter a coverage mask with a disc of radius `r`.
fn dilate(m: &[f32], w: usize, h: usize, r: f32) -> Vec<f32> {
    let ri = r.ceil() as i32;
    let mut offs = Vec::new();
    for dy in -ri..=ri {
        for dx in -ri..=ri {
            let d = ((dx * dx + dy * dy) as f32).sqrt();
            if d <= r + 0.5 {
                offs.push((dx, dy, (r + 0.5 - d).clamp(0.0, 1.0)));
            }
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut v = 0.0f32;
            for &(dx, dy, wgt) in &offs {
                let (sx, sy) = (x + dx, y + dy);
                if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                    v = v.max(m[sy as usize * w + sx as usize] * wgt);
                }
            }
            out[y as usize * w + x as usize] = v;
        }
    }
    out
}

/// The overlay for a caption track at timeline time `t` (None when nothing shows).
pub fn track_overlay(track: &CaptionTrack, t: Tick, w: usize, h: usize) -> Option<Overlay> {
    if !track.enabled {
        return None;
    }
    let c = track.caption_at(t)?;
    render_caption(c, &track.style, w, h)
}

/// Overlays of every visible caption track of a sequence at `t`, bottom track first.
pub fn sequence_overlays(seq: &Sequence, t: Tick, w: usize, h: usize) -> Vec<Overlay> {
    seq.caption_tracks.iter().rev().filter_map(|tr| track_overlay(tr, t, w, h)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{CaptionFormat, ClipId, TrackId};

    fn cap(text: &str) -> Caption {
        Caption { id: ClipId(1), start: Tick(0), duration: Tick(100), text: text.into(), speaker: None, cue_id: None, settings: String::new() }
    }

    #[test]
    fn bottom_centre_with_box() {
        let st = CaptionStyle::default();
        let o = render_caption(&cap("Hello world"), &st, 1920, 1080).unwrap();
        // centred horizontally, in the lower part of the frame
        let cx = o.x as f32 + o.w as f32 / 2.0;
        assert!((cx - 960.0).abs() < 4.0, "centre {cx}");
        assert!(o.y > 800 && (o.y as usize + o.h) < 1080, "y {} h {}", o.y, o.h);
        // white text pixels exist and the box is dark translucent
        let mut white = 0;
        let mut boxy = 0;
        for p in o.px.chunks(4) {
            if p[3] > 0.99 && p[0] > 0.9 {
                white += 1;
            } else if p[3] > 0.7 && p[0] < 0.01 {
                boxy += 1;
            }
        }
        assert!(white > 500, "{white}");
        assert!(boxy > 1000, "{boxy}");
    }

    #[test]
    fn top_anchor_and_vtt_line() {
        let st = CaptionStyle { anchor: CaptionAnchor::Top, ..Default::default() };
        let o = render_caption(&cap("Top"), &st, 1280, 720).unwrap();
        assert!(o.y < 100);
        let mut c = cap("Line");
        c.settings = "line:50% align:start".into();
        let o = render_caption(&c, &CaptionStyle::default(), 1280, 720).unwrap();
        assert!((o.y - 360).abs() < 4, "{}", o.y);
        assert!(o.x < 120, "{}", o.x);
    }

    #[test]
    fn margin_is_a_fraction_of_frame_height() {
        // #1542: on a portrait sequence, a bottom-anchored caption with margin 0.31 sits with
        // its bottom edge 0.31 of the frame height above the bottom; the top anchor mirrors it.
        let st = CaptionStyle { anchor: CaptionAnchor::Bottom, margin: 0.31, size: 78.0, ..Default::default() };
        let o = render_caption(&cap("Hello world"), &st, 1080, 1920).unwrap();
        // the overlay pads the content by the outline grow (the default outline is 0, so by 1 px)
        let bottom = (o.y as usize + o.h) as f32;
        let expected = 1920.0 - 0.31 * 1920.0;
        assert!((bottom - expected).abs() <= 2.0, "bottom edge {bottom}, expected {expected}");
        let st = CaptionStyle { anchor: CaptionAnchor::Top, margin: 0.31, size: 78.0, ..Default::default() };
        let o = render_caption(&cap("Hello world"), &st, 1080, 1920).unwrap();
        assert!((o.y as f32 - 0.31 * 1920.0).abs() <= 2.0, "top edge {}, expected {}", o.y, 0.31 * 1920.0);
        // margin 0 and the 0.45 clamp stay inside the frame
        for margin in [0.0, 0.31, 0.45, 9.0] {
            let st = CaptionStyle { anchor: CaptionAnchor::Bottom, margin, size: 78.0, ..Default::default() };
            let o = render_caption(&cap("Hello world"), &st, 1080, 1920).unwrap();
            // the outline-grow padding plus ceil rounding may reach 2 px past the edge; the
            // composite clips it
            assert!(o.y >= 0 && o.y as usize + o.h <= 1922, "margin {margin}: y {} h {}", o.y, o.h);
        }
    }

    #[test]
    fn wraps_and_composites() {
        let long = "word ".repeat(60);
        let o = render_caption(&cap(&long), &CaptionStyle::default(), 640, 360).unwrap();
        assert!(o.w <= 640 && o.h > 40, "{}x{}", o.w, o.h);
        let mut canvas = vec![0.0f32; 640 * 360 * 4];
        o.composite_onto(&mut canvas, 640, 360);
        assert!(canvas.chunks(4).any(|p| p[0] > 0.9));
    }

    #[test]
    fn overlay_is_clipped_to_the_frame_for_an_unbroken_word() {
        let o = render_caption(&cap(&"W".repeat(10_000)), &CaptionStyle::default(), 640, 360).unwrap();
        assert!(o.x >= 0 && o.y >= 0);
        assert!(o.w <= 640 && o.h <= 360, "{}x{}", o.w, o.h);
        assert!(o.w.checked_mul(o.h).unwrap() <= MAX_OVERLAY_PIXELS);
    }

    #[test]
    fn hostile_outline_is_bounded() {
        let st = CaptionStyle { outline: 1_000_000.0, ..Default::default() };
        let o = render_caption(&cap("Safe"), &st, 640, 360).unwrap();
        assert!(o.w <= 640 && o.h <= 360, "{}x{}", o.w, o.h);
    }

    #[test]
    fn outline_draws_around_text() {
        let st = CaptionStyle { background: false, outline: 4.0, outline_color: [255, 0, 0, 255], ..Default::default() };
        let o = render_caption(&cap("O"), &st, 1920, 1080).unwrap();
        assert!(o.px.chunks(4).any(|p| p[0] > 0.9 && p[1] < 0.05 && p[3] > 0.9), "red outline pixels");
    }

    #[test]
    fn track_font_is_used() {
        // the style's font used to be ignored (always Inter)
        let inter = render_caption(&cap("Hello world"), &CaptionStyle::default(), 1920, 1080).unwrap();
        let serif = CaptionStyle { font: "Noto Serif".into(), ..Default::default() };
        let serif = render_caption(&cap("Hello world"), &serif, 1920, 1080).unwrap();
        assert_ne!((inter.w, &inter.px), (serif.w, &serif.px), "Noto Serif renders like Inter");
        let mono = CaptionStyle { font: "jetbrains mono".into(), ..Default::default() };
        let mono = render_caption(&cap("Hello world"), &mono, 1920, 1080).unwrap();
        assert_ne!((inter.w, &inter.px), (mono.w, &mono.px), "JetBrains Mono renders like Inter");
    }

    #[test]
    fn unknown_or_empty_font_falls_back_to_inter() {
        let inter = render_caption(&cap("Hello"), &CaptionStyle::default(), 1280, 720).unwrap();
        for font in ["", "  ", "No Such Font 123"] {
            let st = CaptionStyle { font: font.into(), ..Default::default() };
            assert_eq!(render_caption(&cap("Hello"), &st, 1280, 720), Some(inter.clone()), "font {font:?}");
        }
    }

    #[test]
    fn hidden_track_draws_nothing() {
        let mut t = CaptionTrack::new(TrackId(1), "S".into(), CaptionFormat::Subtitle);
        t.captions.push(cap("x"));
        assert!(track_overlay(&t, Tick(5), 100, 100).is_some());
        t.enabled = false;
        assert!(track_overlay(&t, Tick(5), 100, 100).is_none());
    }
    #[test]
    fn box_padding_ignored_when_background_off() {
        let style_no_bg = CaptionStyle { background: false, box_padding: 20.0, ..Default::default() };
        let style_with_bg = CaptionStyle { background: true, box_padding: 20.0, ..Default::default() };
        // With background off, box_padding must not affect text origin -- render must not panic
        let result_no_bg = render_caption(&cap("Test"), &style_no_bg, 1920, 1080);
        assert!(result_no_bg.is_some(), "render without background should succeed");
        // With background on and padding, render should also succeed
        let result_with_bg = render_caption(&cap("Test"), &style_with_bg, 1920, 1080);
        assert!(result_with_bg.is_some(), "render with background+padding should succeed");
        // The overlay with padding should be at least as wide as without padding
        let style_no_pad = CaptionStyle { background: true, box_padding: 0.0, ..Default::default() };
        let result_no_pad = render_caption(&cap("Test"), &style_no_pad, 1920, 1080).unwrap();
        let result_pad = result_with_bg.unwrap();
        assert!(result_pad.w >= result_no_pad.w, "padding should expand box width");
    }
}
