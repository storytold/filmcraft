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
///
/// The overlay contains the background, text shadow, outline and fill, composited in
/// that order. Style dimensions scale from a 1080-line frame; the raster budget
/// scales with frame area. Excessive shadow padding falls back to the unshadowed caption. Invalid
/// base geometry or a base overlay exceeding the budget returns `None`.
pub fn render_caption(c: &Caption, style: &CaptionStyle, w: usize, h: usize) -> Option<Overlay> {
    // Bound work before converting file-derived geometry or allocating raster masks.
    if w == 0
        || h == 0
        || w > 16384
        || h > 16384
        || c.text.len() > 65536
        || ![style.size, style.line_spacing, style.margin, style.outline].iter().all(|v| v.is_finite())
    {
        return None;
    }
    let scale = h as f32 / 1080.0;
    let px = (style.size * scale).max(4.0);
    if px > 16384.0 {
        return None;
    }
    let max_w = w as f32 * 0.9;
    let ts = caption_style(&style.font, px);
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
    let lh = px * style.line_spacing.max(0.8);
    let pad_x = (px * 0.3).round();
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
    let outline = (style.outline * scale).max(0.0);
    let shadow_on = style.shadow && style.shadow_opacity > 0.0 && style.shadow_color[3] > 0;
    let (dx, dy, spread, sigma) = if shadow_on {
        if ![style.shadow_opacity, style.shadow_angle, style.shadow_distance, style.shadow_size, style.shadow_blur].iter().all(|v| v.is_finite()) {
            return None;
        }
        let angle = (style.shadow_angle.rem_euclid(360.0) - 90.0).to_radians();
        let distance = style.shadow_distance.clamp(0.0, 1000.0) * scale;
        (angle.cos() * distance, angle.sin() * distance, style.shadow_size.clamp(0.0, 1000.0) * scale, style.shadow_blur.clamp(0.0, 1000.0) * scale / 3.0)
    } else {
        (0.0, 0.0, 0.0, 0.0)
    };
    // Three box passes have finite support of three radii, not just sigma. Include that
    // padding and the offset so shadows are never cropped to the original text rectangle.
    let blur_pad = if sigma >= 0.3 { 3.0 * (sigma * sigma + 1.0).sqrt().round().max(1.0) } else { 0.0 };
    let grow = outline.ceil() + 1.0 + spread.ceil() + blur_pad + dx.abs().max(dy.abs()).ceil();
    let bx0 = xs.iter().zip(&widths).map(|(x, _)| x - pad_x - grow).fold(f32::MAX, f32::min).floor();
    let bx1 = xs.iter().zip(&widths).map(|(x, lw)| x + lw + pad_x + grow).fold(f32::MIN, f32::max).ceil();
    let by0 = (top - grow).floor();
    let by1 = (top + block_h + grow).ceil();
    if ![bx0, bx1, by0, by1].iter().all(|v| v.is_finite() && v.abs() <= 131072.0) {
        return None;
    }
    let (ox, oy) = (bx0 as i32, by0 as i32);
    let ow = (bx1 - bx0).max(1.0) as usize;
    let oh = (by1 - by0).max(1.0) as usize;
    let pixels = ow.checked_mul(oh)?;
    // A fixed 4 Mi-pixel ceiling dropped ordinary multiline captions at 8K.
    // Allow up to one frame of coverage (at least 4 Mi-pixels), capped at 32 Mi-pixels.
    let budget = w.checked_mul(h)?.clamp(4_194_304, 33_554_432);
    if ow > 16384 || oh > 16384 || pixels > budget {
        if shadow_on {
            // Only one retry: the disabled shadow takes the base-caption path.
            return render_caption(c, &CaptionStyle { shadow: false, ..style.clone() }, w, h);
        }
        return None;
    }
    let mut cover = vec![0.0f32; ow * oh];
    let mut bg = vec![0.0f32; ow * oh];
    for (i, line) in lines.iter().enumerate() {
        let lt = top + lh * i as f32;
        if style.background && style.background_color[3] > 0 {
            let (x0, x1) = (xs[i] - pad_x, xs[i] + widths[i] + pad_x);
            let (y0, y1) = (lt, lt + lh);
            for y in (y0.floor() as i32).max(oy)..(y1.ceil() as i32).min(oy + oh as i32) {
                let fy = ((y as f32 + 1.0).min(y1) - (y as f32).max(y0)).clamp(0.0, 1.0);
                for x in (x0.floor() as i32).max(ox)..(x1.ceil() as i32).min(ox + ow as i32) {
                    let fx = ((x as f32 + 1.0).min(x1) - (x as f32).max(x0)).clamp(0.0, 1.0);
                    let j = (y - oy) as usize * ow + (x - ox) as usize;
                    bg[j] = (bg[j] + fx * fy).min(1.0);
                }
            }
        }
        let baseline = lt + (lh - (asc + desc)) / 2.0 + asc;
        let l = layout(line, &ts, &ParagraphStyle::default());
        let mut m = filmcraft_text::Mask { w: ow, h: oh, a: std::mem::take(&mut cover) };
        render::draw(&l, &render::at(xs[i], baseline.round()), &mut m, (ox, oy));
        cover = m.a;
    }
    let stroke = if outline > 0.0 { dilate_text_bounds(&cover, ow, oh, outline) } else { Vec::new() };
    let shadow = if shadow_on {
        let mut mask = filmcraft_text::Mask { w: ow, h: oh, a: if stroke.is_empty() { cover.clone() } else { stroke.clone() } };
        if spread > 0.0 {
            mask = filmcraft_text::mask::stroke(&mask, spread, filmcraft_text::mask::StrokeKind::Outer);
        }
        mask = filmcraft_text::mask::blur(&mask, sigma);
        Some(filmcraft_text::mask::offset(&mask, dx.round() as i32, dy.round() as i32))
    } else {
        None
    };
    let sc = lin(style.shadow_color).map(|v| v * (style.shadow_opacity / 100.0).clamp(0.0, 1.0));
    let (tc, bc, oc) = (lin(style.color), lin(style.background_color), lin(style.outline_color));
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
        if let Some(mask) = &shadow {
            over(&mut p, sc, mask.a.get(j).copied().unwrap_or(0.0).clamp(0.0, 1.0));
        }
        if !stroke.is_empty() {
            over(&mut p, oc, stroke[j]);
        }
        over(&mut p, tc, cover[j]);
        out[j * 4..j * 4 + 4].copy_from_slice(&p);
    }
    Some(Overlay { x: ox, y: oy, w: ow, h: oh, px: out })
}

/// Dilate only the glyph bounds plus outline support, not the empty shadow padding.
/// Cropping zero coverage preserves the max-filter result while keeping distant
/// shadows from multiplying the outline's neighbourhood scans.
fn dilate_text_bounds(m: &[f32], w: usize, h: usize, r: f32) -> Vec<f32> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (i, &a) in m.iter().enumerate() {
        if a > 0.0 {
            x0 = x0.min(i % w);
            y0 = y0.min(i / w);
            x1 = x1.max(i % w + 1);
            y1 = y1.max(i / w + 1);
        }
    }
    let mut out = vec![0.0; m.len()];
    if x0 >= x1 || y0 >= y1 {
        return out;
    }
    let pad = r.ceil() as usize;
    let (x0, y0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
    let (x1, y1) = (x1.saturating_add(pad).min(w), y1.saturating_add(pad).min(h));
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut cropped = Vec::with_capacity(cw * ch);
    for y in y0..y1 {
        cropped.extend_from_slice(&m[y * w + x0..y * w + x1]);
    }
    let stroke = dilate(&cropped, cw, ch, r);
    for y in 0..ch {
        out[(y0 + y) * w + x0..(y0 + y) * w + x1].copy_from_slice(&stroke[y * cw..(y + 1) * cw]);
    }
    out
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

    /// Composite a synthetic caption onto a transparent frame for pixel comparisons.
    fn canvas(style: &CaptionStyle, w: usize, h: usize) -> Vec<f32> {
        let o = render_caption(&cap("Shadow"), style, w, h).unwrap();
        let mut pixels = vec![0.0; w * h * 4];
        o.composite_onto(&mut pixels, w, h);
        pixels
    }

    /// Verify shadow colour and opacity at cardinal offsets across two frame sizes.
    #[test]
    fn shadow_color_opacity_direction_and_resolution() {
        let mut st = CaptionStyle {
            background: false,
            anchor: CaptionAnchor::Middle,
            shadow: true,
            shadow_color: [255, 0, 0, 255],
            shadow_opacity: 50.0,
            shadow_angle: 90.0,
            shadow_distance: 120.0,
            shadow_size: 0.0,
            shadow_blur: 0.0,
            ..Default::default()
        };
        // A sharp, separated red shadow must be the text's coverage translated by a
        // resolution-scaled distance, with half its alpha. Test both directions and scales.
        for (w, h) in [(960, 540), (1920, 1080)] {
            let distance = (120 * h / 1080) as isize;
            for (angle, dx, dy) in [(90.0, distance, 0), (180.0, 0, distance), (270.0, -distance, 0), (0.0, 0, -distance)] {
                st.shadow_angle = angle;
                let shadowed = canvas(&st, w, h);
                let plain = canvas(&CaptionStyle { shadow: false, ..st.clone() }, w, h);
                let mut checked = 0;
                for y in 0..h {
                    for x in 0..w {
                        let a = plain[(y * w + x) * 4 + 3];
                        let (sx, sy) = (x as isize + dx, y as isize + dy);
                        if a > 0.0 && sx >= 0 && sy >= 0 && sx < w as isize && sy < h as isize {
                            let i = (sy as usize * w + sx as usize) * 4;
                            if plain[i + 3] == 0.0 {
                                assert!((shadowed[i] - a * 0.5).abs() < 0.001);
                                assert_eq!(shadowed[i + 1], 0.0);
                                assert!((shadowed[i + 3] - a * 0.5).abs() < 0.001);
                                checked += 1;
                            }
                        }
                    }
                }
                assert!(checked > 100);
            }
        }
    }

    /// Check spread, blur support and exact pixel identity for invisible shadows.
    #[test]
    fn shadow_spread_blur_and_disabled_identity() {
        let mut st = CaptionStyle {
            background: false,
            shadow: true,
            shadow_distance: 100.0,
            shadow_blur: 0.0,
            shadow_opacity: 100.0,
            shadow_color: [255, 0, 0, 255],
            ..Default::default()
        };
        let c = cap("Test");
        let sharp = render_caption(&c, &st, 960, 540).unwrap();
        let red_mass = |o: &Overlay| o.px.chunks(4).map(|p| p[0] - p[1]).sum::<f32>();
        st.shadow_size = 10.0;
        let spread = render_caption(&c, &st, 960, 540).unwrap();
        assert!(red_mass(&spread) > red_mass(&sharp));
        st.shadow_blur = 30.0;
        let soft = render_caption(&c, &st, 960, 540).unwrap();
        assert!(soft.w > spread.w && soft.h > spread.h);
        assert!(soft.px.chunks(4).any(|p| p[0] > 0.01 && p[0] < 0.5 && p[1] == 0.0));
        // Full blur support is in the overlay, not clipped at its boundary.
        for x in 0..soft.w {
            assert!(soft.px[x * 4 + 3].abs() < 0.001);
            assert!(soft.px[((soft.h - 1) * soft.w + x) * 4 + 3].abs() < 0.001);
        }
        st.shadow = false;
        let disabled = render_caption(&c, &st, 960, 540);
        st.shadow = true;
        st.shadow_opacity = 0.0;
        assert_eq!(render_caption(&c, &st, 960, 540), disabled);
        st.shadow_opacity = 100.0;
        st.shadow_color[3] = 0;
        assert_eq!(render_caption(&c, &st, 960, 540), disabled);
    }

    /// Reject non-finite geometry and bound extreme dimensions without panicking.
    #[test]
    fn hostile_shadow_geometry_is_bounded() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let st = CaptionStyle { shadow: true, shadow_distance: bad, ..Default::default() };
            assert!(std::panic::catch_unwind(|| render_caption(&cap("x"), &st, 640, 360)).unwrap().is_none());
        }
        let st = CaptionStyle { shadow: true, shadow_blur: f32::MAX, shadow_size: f32::MAX, ..Default::default() };
        assert!(std::panic::catch_unwind(|| render_caption(&cap("x"), &st, 320, 180)).unwrap().is_some());
        assert!(render_caption(&cap("x"), &st, usize::MAX, usize::MAX).is_none());
    }

    /// Ordinary multiline captions must survive the old fixed mask ceiling at 8K.
    #[test]
    fn multiline_captions_survive_hd_4k_and_8k() {
        let line = "A multiline caption stays readable at every output resolution";
        let c = cap(&format!("{line}\n{line}\n{line}"));
        for (w, h) in [(1920, 1080), (3840, 2160), (7680, 4320)] {
            let plain = render_caption(&c, &CaptionStyle { background: false, ..Default::default() }, w, h).unwrap();
            let shadow = render_caption(&c, &CaptionStyle { background: false, shadow: true, ..Default::default() }, w, h).unwrap();
            if h == 4320 {
                assert!(plain.w * plain.h > 4_194_304, "regression must exceed the former limit");
            }
            assert!(shadow.w > plain.w && shadow.h > plain.h, "default shadow is retained at {w}x{h}");
            let white = |o: &Overlay| o.px.chunks(4).filter(|p| p[0] == 1.0 && p[1] == 1.0 && p[2] == 1.0).count();
            assert!(white(&plain) > 100);
            assert_eq!(white(&plain), white(&shadow), "opaque text is retained at {w}x{h}");
        }
    }

    /// Shadow allocation failure must preserve the base caption instead of hiding it.
    #[test]
    fn excessive_shadow_padding_keeps_the_caption() {
        let st = CaptionStyle { shadow: true, shadow_distance: 1000.0, shadow_size: 1000.0, shadow_blur: 1000.0, ..Default::default() };
        let c = cap("Keep this caption");
        let plain = render_caption(&c, &CaptionStyle { shadow: false, ..st.clone() }, 1920, 1080);
        assert!(plain.is_some());
        assert_eq!(render_caption(&c, &st, 1920, 1080), plain);
    }

    /// Cropping the outline work preserves fractional mask coverage and edge pixels.
    #[test]
    fn cropped_outline_matches_full_disc_filter() {
        let (w, h) = (80, 50);
        for origin in [0, 20 * w + 30, w * h - 2] {
            let mut m = vec![0.0; w * h];
            m[origin] = 0.4;
            m[origin + 1] = 1.0;
            assert_eq!(dilate_text_bounds(&m, w, h, 2.4), dilate(&m, w, h, 2.4));
        }
    }
}
