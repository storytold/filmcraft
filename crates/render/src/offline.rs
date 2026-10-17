//! The offline slate: what a clip whose media is missing (or was made offline) shows instead of
//! its pictures. Our own design, drawn in code: a dark red field with diagonal hazard stripes, a
//! dark panel with a warning triangle, "MEDIA NOT FOUND", the file name and how to fix it.
//!
//! The drawing function uses the caller's dimensions. Media sources may request a smaller slate
//! for bounded previews. The result is deterministic (tests compare renders against it).

use filmcraft_geom::Vec2;
use filmcraft_text::{Align, ParagraphStyle, TextStyle, layout, render};

use crate::graphics::{paint_mask, premul_linear};
use crate::image::Image;

/// Base colour (sRGB) of the slate field and of its lighter stripes.
pub const FIELD: [u8; 3] = [0x5c, 0x10, 0x16];
const STRIPE: [u8; 3] = [0x6e, 0x17, 0x1e];
const PANEL: [f32; 4] = [0.06, 0.025, 0.03, 0.93];
const AMBER: [f32; 4] = [0.98, 0.72, 0.18, 1.0];

/// Why a clip is offline (picks the headline).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfflineReason {
    /// The file isn't where the project says (moved, renamed, drive not mounted).
    Missing,
    /// Made offline on purpose (Make Offline / Offline All).
    MadeOffline,
    /// The file is there but couldn't be read or decoded.
    Unreadable,
}

impl OfflineReason {
    pub fn headline(self) -> &'static str {
        match self {
            OfflineReason::Missing => "MEDIA NOT FOUND",
            OfflineReason::MadeOffline => "MEDIA SET OFFLINE",
            OfflineReason::Unreadable => "MEDIA UNREADABLE",
        }
    }
}

fn srgb(c: [u8; 3]) -> [f32; 4] {
    premul_linear([c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0])
}

/// Maximum generated offline placeholder size, independent of source metadata.
pub const MAX_OFFLINE_SLATE_SIDE: u32 = 1280;
pub const MAX_OFFLINE_SLATE_PIXELS: u64 = 921_600;

/// Keep untrusted media names from driving an unbounded text layout or raster.
const MAX_SLATE_NAME_BYTES: usize = 512;
const NAME_NOT_SHOWN_ON_SLATE: &str = "File name not shown on slate";

/// Name used only on the raster slate; the project retains the original path.
pub fn slate_display_name(name: &str) -> &str {
    if name.len() > MAX_SLATE_NAME_BYTES || name.chars().any(char::is_control) { NAME_NOT_SHOWN_ON_SLATE } else { name }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidSlateSize {
    pub width: usize,
    pub height: usize,
}

impl std::fmt::Display for InvalidSlateSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "offline slate dimensions {}x{} require nonzero sides at most {} pixels and area at most {} pixels",
            self.width, self.height, MAX_OFFLINE_SLATE_SIDE, MAX_OFFLINE_SLATE_PIXELS
        )
    }
}

impl std::error::Error for InvalidSlateSize {}

/// Draw the slate at `w`×`h` (premultiplied linear-light, opaque).
pub fn slate(w: usize, h: usize, file_name: &str, reason: OfflineReason) -> Result<Image, InvalidSlateSize> {
    let valid = w > 0
        && h > 0
        && w <= MAX_OFFLINE_SLATE_SIDE as usize
        && h <= MAX_OFFLINE_SLATE_SIDE as usize
        && w.checked_mul(h).is_some_and(|pixels| pixels <= MAX_OFFLINE_SLATE_PIXELS as usize);
    if !valid {
        return Err(InvalidSlateSize { width: w, height: h });
    }
    let field = srgb(FIELD);
    let stripe = srgb(STRIPE);
    let period = (h as f32 / 9.0).max(4.0);
    let mut img = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            // 45° stripes, half the period wide
            let d = ((x + y) as f32 / period).fract();
            let c = if d < 0.5 { stripe } else { field };
            let i = (y * w + x) * 4;
            img.px[i..i + 4].copy_from_slice(&c);
        }
    }
    let s = h as f32;
    // centre panel
    let (pw, ph) = ((w as f32 * 0.78).min(s * 1.9), s * 0.46);
    let (px0, py0) = ((w as f32 - pw) / 2.0, (s - ph) / 2.0);
    fill_path(&mut img, &filmcraft_text::Path::rect(px0, py0, px0 + pw, py0 + ph), PANEL);
    // warning triangle with a "!" (two fills: amber outline, panel-coloured inside, amber bar + dot)
    let tri_h = s * 0.13;
    let (tcx, ty0) = (w as f32 / 2.0, py0 + s * 0.045);
    let tri = |inset: f32| {
        let k = inset * 1.9;
        filmcraft_text::Path::polygon(&[
            (tcx, ty0 + k),
            (tcx + tri_h * 0.62 - k * 0.9, ty0 + tri_h - inset),
            (tcx - tri_h * 0.62 + k * 0.9, ty0 + tri_h - inset),
        ])
    };
    fill_path(&mut img, &tri(0.0), AMBER);
    fill_path(&mut img, &tri(tri_h * 0.11), [PANEL[0], PANEL[1], PANEL[2], 1.0]);
    let bar_w = (tri_h * 0.09).max(1.0);
    fill_path(&mut img, &filmcraft_text::Path::rect(tcx - bar_w / 2.0, ty0 + tri_h * 0.36, tcx + bar_w / 2.0, ty0 + tri_h * 0.70), AMBER);
    fill_path(&mut img, &filmcraft_text::Path::rect(tcx - bar_w / 2.0, ty0 + tri_h * 0.76, tcx + bar_w / 2.0, ty0 + tri_h * 0.76 + bar_w), AMBER);
    let cx = w as f32 / 2.0;
    text(&mut img, reason.headline(), "Bold", Vec2::new(cx as f64, (ty0 + tri_h + s * 0.085) as f64), s * 0.062, [1.0, 1.0, 1.0, 1.0], pw * 0.94);
    let display_name = slate_display_name(file_name);
    if !display_name.is_empty() {
        text(&mut img, display_name, "Regular", Vec2::new(cx as f64, (ty0 + tri_h + s * 0.15) as f64), s * 0.036, [1.0, 0.80, 0.80, 1.0], pw * 0.94);
    }
    let hint = match reason {
        OfflineReason::MadeOffline => "Link Media… brings it back",
        _ => "File ▸ Link Media… reconnects it",
    };
    text(&mut img, hint, "Regular", Vec2::new(cx as f64, (ty0 + tri_h + s * 0.205) as f64), s * 0.028, [0.85, 0.72, 0.72, 1.0], pw * 0.94);
    Ok(img)
}

/// The slate as straight sRGB RGBA8 (what offline sources hand to the compositor).
pub fn slate_rgba8(w: usize, h: usize, file_name: &str, reason: OfflineReason) -> Result<Vec<u8>, InvalidSlateSize> {
    Ok(slate(w, h, file_name, reason)?.to_rgba8())
}

fn fill_path(img: &mut Image, p: &filmcraft_text::Path, color: [f32; 4]) {
    let Some(bb) = p.bounds() else { return };
    let (x0, y0) = (bb.0.floor() as i32, bb.1.floor() as i32);
    let m = filmcraft_text::raster::fill(p, (bb.2.ceil() as i32 - x0).max(1) as usize, (bb.3.ceil() as i32 - y0).max(1) as usize, -x0 as f32, -y0 as f32);
    paint_mask(img, &m, x0, y0, color, 1.0);
}

/// One line of text centred on `pos.x` with its baseline at `pos.y`, shrunk to fit `max_w`.
fn text(img: &mut Image, s: &str, style: &str, pos: Vec2, px: f32, color: [f32; 4], max_w: f32) {
    if px < 3.0 {
        return;
    }
    let mut st = TextStyle { family: filmcraft_text::fonts::DEFAULT_FAMILY.into(), style: style.into(), size: px, ..Default::default() };
    let para = ParagraphStyle { align: Align::Center, ..Default::default() };
    let mut l = layout(s, &st, &para);
    let wid = l.bounds[2] - l.bounds[0];
    if wid > max_w && wid > 0.0 {
        st.size = px * max_w / wid;
        l = layout(s, &st, &para);
    }
    let (m, x0, y0) = render::rasterize(&l, &render::at(pos.x as f32, pos.y as f32));
    paint_mask(img, &m, x0, y0, premul_linear(color), 1.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_name_uses_a_bounded_notice_on_the_slate() {
        assert_eq!(slate_display_name("clip.mov"), "clip.mov");
        assert_eq!(slate_display_name(&"x".repeat(MAX_SLATE_NAME_BYTES + 1)), NAME_NOT_SHOWN_ON_SLATE);
        assert_eq!(slate_display_name("many\nlines.mov"), NAME_NOT_SHOWN_ON_SLATE);
        assert_eq!(slate_display_name("control\u{0000}name.mov"), NAME_NOT_SHOWN_ON_SLATE);
        let pixels = slate_rgba8(32, 18, &"\n".repeat(10_000), OfflineReason::Missing).unwrap();
        assert_eq!(pixels.len(), 32 * 18 * 4);
    }
}
