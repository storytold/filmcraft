//! FilmCraft text engine (layer L1).
//!
//! - [`fonts`]: font database — bundled OFL fonts (Inter, JetBrains Mono, Noto Serif), the optional
//!   craft-fonts ([`fonts::CRAFT_FONTS`], Japanese; empty unless built with `CRAFT_FONTS_DIR`) plus faces
//!   discovered by [`fonts::FontSource`]s (system font folders on native platforms; nothing on the
//!   web unless the host registers font data), family/style resolution with synthetic bold/italic
//!   and per-character fallback.
//! - [`layout`]: shaping (harfrust: kerning, ligatures, complex scripts), bidi, line breaking,
//!   paragraph layout, per-character style runs, carets and hit testing; cached.
//! - [`raster`]: our own anti-aliased path rasteriser (linear coverage, so compositing in linear
//!   light is correct) and vector shapes.
//! - [`render`]: glyph outlines and the glyph-mask cache (sub-pixel positioned), drawing layouts
//!   under any affine transform.
//! - [`mask`]: strokes (outer/centre/inner via a Euclidean distance transform), blur, offsets.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod fonts;
pub mod layout;
pub mod mask;
pub mod raster;
pub mod render;
pub mod sfnt;

pub use fonts::{FaceId, Resolved, families, picker_families, resolve};
pub use layout::{Align, Caps, Glyph, Layout, Line, ParagraphStyle, StyleRun, TextStyle, layout, layout_rich, measure};
pub use mask::StrokeKind;
pub use raster::{Mask, Path, Xform};

#[cfg(test)]
mod perf {
    use super::*;

    /// A three-line 1080p title: layout + rasterisation must stay far below a frame (≈ 41 ms at
    /// 24 fps). Cold = first time (shaping + glyph rasterisation); warm = cached.
    #[test]
    fn three_line_title_is_fast() {
        let text = "FILMCRAFT PRESENTS\nA Night Drive Through the City\nDirected by Nobody in Particular";
        let st = TextStyle { size: 96.0, style: "Bold".into(), ..Default::default() };
        let ps = ParagraphStyle { align: Align::Center, ..Default::default() };
        let t0 = std::time::Instant::now();
        let l = layout::layout_uncached(text, &st, &ps);
        let (m, _, _) = render::rasterize(&l, &render::at(960.0, 400.0));
        let cold = t0.elapsed();
        let _ = layout(text, &st, &ps);
        let t1 = std::time::Instant::now();
        let l2 = layout(text, &st, &ps);
        let (m2, _, _) = render::rasterize(&l2, &render::at(960.0, 400.0));
        let warm = t1.elapsed();
        assert_eq!(m.a.len(), m2.a.len());
        eprintln!("3-line 1080p title: cold {cold:?}, warm {warm:?} (mask {}x{})", m.w, m.h);
        // generous bound for debug builds on busy CI machines
        assert!(warm.as_millis() < 40, "warm {warm:?}");
    }
}
