//! Drawing laid-out text into coverage masks.
//!
//! Two paths:
//! - **Glyph cache** (axis-aligned uniform scale, the common case for titles): each glyph is
//!   rasterised once per (face, glyph, size, ¼-pixel horizontal phase, synthetic style) and the
//!   cached masks are added at their pixel positions (sub-pixel positioning in x, whole pixels
//!   in y).
//! - **Outlines** (rotation, skew, non-uniform scale): every glyph outline is transformed into
//!   device space and the whole run is filled once.
//!
//! Synthetic bold grows outlines by 2.5 % of the size per side; synthetic italic slants by 12°.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};

use crate::fonts::{self, FaceId};
use crate::layout::{Glyph, Layout};
use crate::raster::{IDENTITY, Mask, Path, Xform, apply, compose, fill, fill_into, xform_scale};

/// Slant of synthetic italic (tan 12°).
pub const SYNTH_ITALIC_SLANT: f32 = 0.2126;
/// Synthetic bold outline offset as a fraction of the font size.
pub const SYNTH_BOLD: f32 = 0.025;

#[derive(Clone, Copy, Debug)]
enum Cmd {
    M(f32, f32),
    L(f32, f32),
    Q(f32, f32, f32, f32),
    C(f32, f32, f32, f32, f32, f32),
    Z,
}

struct Rec(Vec<Cmd>);

impl OutlinePen for Rec {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(Cmd::M(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(Cmd::L(x, y));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.push(Cmd::Q(cx0, cy0, x, y));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.push(Cmd::C(cx0, cy0, cx1, cy1, x, y));
    }
    fn close(&mut self) {
        self.0.push(Cmd::Z);
    }
}

/// Outline commands in font units (y up), cached per (face, glyph).
fn outline(face: FaceId, gid: u32) -> Option<Arc<Vec<Cmd>>> {
    static C: OnceLock<Mutex<HashMap<(FaceId, u32), Option<Arc<Vec<Cmd>>>>>> = OnceLock::new();
    let c = C.get_or_init(Default::default);
    if let Some(v) = c.lock().unwrap_or_else(|e| e.into_inner()).get(&(face, gid)) {
        return v.clone();
    }
    let f = fonts::face(face);
    let v = f.font().and_then(|font| {
        let g = font.outline_glyphs().get(GlyphId::new(gid))?;
        let mut rec = Rec(Vec::new());
        g.draw(DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut rec).ok()?;
        Some(Arc::new(rec.0))
    });
    let mut m = c.lock().unwrap_or_else(|e| e.into_inner());
    if m.len() > 20_000 {
        m.clear();
    }
    m.insert((face, gid), v.clone());
    v
}

/// The glyph's outline as a path: `m` maps glyph-local pixels (origin on the baseline, y down,
/// at the glyph's size) to the output.
pub fn glyph_path(g: &Glyph, m: &Xform) -> Path {
    let mut p = Path::new();
    let Some(cmds) = outline(g.face, g.id) else { return p };
    let k = g.size / fonts::face(g.face).units_per_em();
    let slant = if g.synth_italic { SYNTH_ITALIC_SLANT } else { 0.0 };
    // font units (y up) → glyph px (y down), slanted
    let local: Xform = [k, 0.0, slant * k, -k, 0.0, 0.0];
    let t = compose(m, &local);
    p.detail = xform_scale(m).max(0.05);
    let pt = |x: f32, y: f32| apply(&t, x, y);
    for c in cmds.iter() {
        match *c {
            Cmd::M(x, y) => {
                let (a, b) = pt(x, y);
                p.move_to(a, b)
            }
            Cmd::L(x, y) => {
                let (a, b) = pt(x, y);
                p.line_to(a, b)
            }
            Cmd::Q(cx, cy, x, y) => {
                let (c0, c1) = pt(cx, cy);
                let (a, b) = pt(x, y);
                p.quad_to(c0, c1, a, b)
            }
            Cmd::C(c0x, c0y, c1x, c1y, x, y) => {
                let (p0, p1) = pt(c0x, c0y);
                let (q0, q1) = pt(c1x, c1y);
                let (a, b) = pt(x, y);
                p.cubic_to(p0, p1, q0, q1, a, b)
            }
            Cmd::Z => p.close(),
        }
    }
    p.close();
    if g.synth_bold {
        p.embolden(g.size * SYNTH_BOLD * xform_scale(m));
    }
    p
}

/// A rasterised glyph: coverage placed with its top-left at `(left, top)` relative to the
/// pen's whole-pixel position.
#[derive(Debug)]
pub struct GlyphMask {
    pub mask: Mask,
    pub left: i32,
    pub top: i32,
}

type GKey = (FaceId, u32, u32, u8, bool, bool);

fn glyph_cache() -> &'static Mutex<HashMap<GKey, Option<Arc<GlyphMask>>>> {
    static C: OnceLock<Mutex<HashMap<GKey, Option<Arc<GlyphMask>>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Rasterise glyph `g` at pixel size `px` with horizontal phase `frac` (0..1, quantised to ¼).
pub fn glyph_mask(g: &Glyph, px: f32, frac: f32) -> Option<Arc<GlyphMask>> {
    let size_q = (px * 8.0).round().max(1.0) as u32;
    let frac_q = ((frac.rem_euclid(1.0) * 4.0).round() as u8) % 4;
    let key = (g.face, g.id, size_q, frac_q, g.synth_bold, g.synth_italic);
    if let Some(v) = glyph_cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return v.clone();
    }
    let px = size_q as f32 / 8.0;
    let fq = frac_q as f32 / 4.0;
    let gg = Glyph { x: 0.0, y: 0.0, size: px, ..*g };
    let path = glyph_path(&gg, &[1.0, 0.0, 0.0, 1.0, fq, 0.0]);
    let v = path.bounds().map(|(x0, y0, x1, y1)| {
        let left = x0.floor() as i32;
        let top = y0.floor() as i32;
        let w = (x1.ceil() as i32 - left).max(1) as usize;
        let h = (y1.ceil() as i32 - top).max(1) as usize;
        Arc::new(GlyphMask { mask: fill(&path, w, h, -left as f32, -top as f32), left, top })
    });
    let mut m = glyph_cache().lock().unwrap_or_else(|e| e.into_inner());
    if m.len() > 16_384 {
        m.clear();
    }
    m.insert(key, v.clone());
    v
}

/// Whether `m` is a uniform positive scale plus translation (the glyph-cache path applies).
pub fn is_axis_uniform(m: &Xform) -> bool {
    m[1].abs() < 1e-6 && m[2].abs() < 1e-6 && m[0] > 0.0 && (m[0] - m[3]).abs() < 1e-4 * m[0].max(1.0)
}

/// The whole layout (glyphs + underlines) as one device-space path.
pub fn layout_path(l: &Layout, m: &Xform) -> Path {
    layout_path_run(l, m, None)
}

/// The glyphs and underlines set in style `run` (all with None) as one device-space path.
pub fn layout_path_run(l: &Layout, m: &Xform, run: Option<u16>) -> Path {
    let mut p = Path::new();
    for g in l.glyphs.iter().filter(|g| run.is_none_or(|r| g.run == r)) {
        let gm = compose(m, &[1.0, 0.0, 0.0, 1.0, g.x, g.y]);
        p.extend(&glyph_path(g, &gm));
    }
    for (i, u) in l.underlines.iter().enumerate() {
        if run.is_none_or(|r| l.underline_runs.get(i).copied().unwrap_or(0) == r) {
            p.extend(&Path::rect(u[0], u[1], u[2], u[3]).transformed(m));
        }
    }
    p
}

/// Device-space bounding box of the layout's ink under `m` (logical bounds padded for overhangs).
pub fn device_bounds(l: &Layout, m: &Xform) -> [f32; 4] {
    let b = l.bounds;
    let pad = l.glyphs.iter().map(|g| g.size).fold(0.0f32, f32::max) * 0.35;
    let pts = [(b[0] - pad, b[1] - pad), (b[2] + pad, b[1] - pad), (b[0] - pad, b[3] + pad), (b[2] + pad, b[3] + pad)];
    let mut o = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for (x, y) in pts {
        let (a, c) = apply(m, x, y);
        o = [o[0].min(a), o[1].min(c), o[2].max(a), o[3].max(c)];
    }
    o
}

/// Add the coverage of layout `l` under transform `m` into `out` (whose pixel (0,0) is device
/// pixel `origin`).
pub fn draw(l: &Layout, m: &Xform, out: &mut Mask, origin: (i32, i32)) {
    draw_run(l, m, out, origin, None)
}

/// [`draw`] limited to the glyphs and underlines set in style `run` (see
/// [`Glyph::run`](crate::Glyph::run)); None draws everything.
pub fn draw_run(l: &Layout, m: &Xform, out: &mut Mask, origin: (i32, i32), run: Option<u16>) {
    let (ox, oy) = origin;
    let keep = |r: u16| run.is_none_or(|x| x == r);
    if is_axis_uniform(m) {
        let s = m[0];
        for g in l.glyphs.iter().filter(|g| keep(g.run)) {
            let (x, y) = apply(m, g.x, g.y);
            let px = g.size * s;
            if px < 0.5 {
                continue;
            }
            let xi = x.floor();
            if let Some(gm) = glyph_mask(g, px, x - xi) {
                out.add(&gm.mask, xi as i32 + gm.left - ox, y.round() as i32 + gm.top - oy);
            }
        }
        if !l.underlines.is_empty() {
            let mut p = Path::new();
            for (i, u) in l.underlines.iter().enumerate() {
                if keep(l.underline_runs.get(i).copied().unwrap_or(0)) {
                    p.extend(&Path::rect(u[0], u[1], u[2], u[3]).transformed(m));
                }
            }
            if !p.is_empty() {
                fill_into(&p, out, -ox as f32, -oy as f32);
            }
        }
    } else {
        let p = layout_path_run(l, m, run);
        fill_into(&p, out, -ox as f32, -oy as f32);
    }
}

/// Rasterise a layout into a tight mask; returns `(mask, x, y)` with the mask's device position.
pub fn rasterize(l: &Layout, m: &Xform) -> (Mask, i32, i32) {
    let b = device_bounds(l, m);
    let (x0, y0) = (b[0].floor() as i32 - 1, b[1].floor() as i32 - 1);
    let (w, h) = ((b[2].ceil() as i32 + 1 - x0).max(1) as usize, (b[3].ceil() as i32 + 1 - y0).max(1) as usize);
    let mut mask = Mask::new(w, h);
    draw(l, m, &mut mask, (x0, y0));
    (mask, x0, y0)
}

/// A one-line sample of `text` set in `family`'s regular face at `px` pixels, vertically centred
/// in a `w`×`h` mask and clipped at its right edge, for font menus that show each family in its
/// own face. None when the family is not registered or its file is unreadable, when the face
/// lacks a glyph for one of the characters, or when it draws no ink. The face's file is read for
/// the sample alone and dropped, so browsing a long font list does not keep every installed font
/// in memory.
pub fn preview(family: &str, text: &str, px: f32, w: usize, h: usize) -> Option<Mask> {
    if !(px > 0.0 && px < 1000.0) || w == 0 || h == 0 || w > 4096 || h > 1024 {
        return None;
    }
    let face = fonts::regular_face(family)?;
    sample(&face, text, px, w, h)
}

fn sample(face: &fonts::Face, text: &str, px: f32, w: usize, h: usize) -> Option<Mask> {
    let path = face
        .with_font_transient(|font| {
            let size = Size::new(px);
            let loc = LocationRef::default();
            let charmap = font.charmap();
            let advances = font.glyph_metrics(size, loc);
            let outlines = font.outline_glyphs();
            let m = font.metrics(size, loc);
            // ascent up, descent (negative) down: centre the line box in the mask
            let mut pen = Pen { path: Path::new(), x: 0.0, baseline: (h as f32 + m.ascent + m.descent) / 2.0 };
            for c in text.chars() {
                if pen.x > w as f32 {
                    break;
                }
                let gid = charmap.map(c).filter(|g| g.to_u32() != 0);
                if c.is_whitespace() {
                    pen.x += gid.and_then(|g| advances.advance_width(g)).unwrap_or(px * 0.25);
                    continue;
                }
                let gid = gid?;
                if let Some(g) = outlines.get(gid) {
                    // a glyph that fails to draw leaves a gap rather than dropping the sample
                    let _ = g.draw(DrawSettings::unhinted(size, loc), &mut pen);
                }
                pen.x += advances.advance_width(gid).unwrap_or(0.0);
            }
            Some(pen.path)
        })
        .flatten()?;
    let mask = fill(&path, w, h, 0.0, 0.0);
    mask.a.iter().any(|a| *a > 0.0).then_some(mask)
}

/// Draws outlines at their pixel size (y up) into a path (y down) at the pen position.
struct Pen {
    path: Path,
    x: f32,
    baseline: f32,
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to(self.x + x, self.baseline - y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(self.x + x, self.baseline - y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.path.quad_to(self.x + cx0, self.baseline - cy0, self.x + x, self.baseline - y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.path.cubic_to(self.x + cx0, self.baseline - cy0, self.x + cx1, self.baseline - cy1, self.x + x, self.baseline - y);
    }
    fn close(&mut self) {
        self.path.close();
    }
}

/// Translate-only transform helper.
pub fn at(x: f32, y: f32) -> Xform {
    let mut t = IDENTITY;
    t[4] = x;
    t[5] = y;
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{ParagraphStyle, TextStyle, layout};

    fn ink(m: &Mask) -> f32 {
        m.a.iter().sum()
    }

    #[test]
    fn h_has_two_stems() {
        let l = layout("H", &TextStyle { size: 40.0, ..Default::default() }, &ParagraphStyle::default());
        let g = glyph_mask(&l.glyphs[0], 40.0, 0.0).unwrap();
        let y = g.mask.h / 4;
        let row: Vec<f32> = (0..g.mask.w).map(|x| g.mask.get(x, y)).collect();
        assert!(row[1] > 0.9 && row[g.mask.w - 2] > 0.9, "{row:?}");
        assert!(row[g.mask.w / 2] < 0.1, "{row:?}");
        assert!(g.top < 0, "rises above the baseline");
    }

    #[test]
    fn cached_and_outline_paths_agree() {
        let l = layout("Title 42", &TextStyle { size: 48.0, ..Default::default() }, &ParagraphStyle::default());
        let fast = rasterize(&l, &at(20.25, 60.0));
        // a tiny rotation forces the outline path
        let a = 1e-4f32;
        let slow = rasterize(&l, &[a.cos(), a.sin(), -a.sin(), a.cos(), 20.25, 60.0]);
        let (fi, si) = (ink(&fast.0), ink(&slow.0));
        assert!((fi - si).abs() / fi < 0.02, "{fi} vs {si}");
    }

    #[test]
    fn synthetic_styles() {
        let base = TextStyle { size: 60.0, ..Default::default() };
        let l = layout("Hello", &base, &ParagraphStyle::default());
        let b = layout("Hello", &TextStyle { faux_bold: true, ..base.clone() }, &ParagraphStyle::default());
        assert!(ink(&rasterize(&b, &IDENTITY).0) > ink(&rasterize(&l, &IDENTITY).0) * 1.1);
        let i = layout("l", &TextStyle { faux_italic: true, ..base.clone() }, &ParagraphStyle::default());
        let (mi, _, _) = rasterize(&i, &IDENTITY);
        // the top of an italic l is further right than its bottom
        let row_centre = |y: usize| {
            let r: Vec<f32> = (0..mi.w).map(|x| mi.get(x, y)).collect();
            let s: f32 = r.iter().sum();
            r.iter().enumerate().map(|(x, v)| x as f32 * v).sum::<f32>() / s.max(1e-6)
        };
        let rows: Vec<usize> = (0..mi.h).filter(|&y| (0..mi.w).any(|x| mi.get(x, y) > 0.5)).collect();
        assert!(row_centre(rows[2]) > row_centre(rows[rows.len() - 3]) + 3.0);
    }

    /// #1536: font menus show each family's name in its own face.
    #[test]
    fn previews_set_the_text_in_the_family_face() {
        let sans = preview("Inter", "Inter Sample", 16.0, 160, 22).expect("bundled sans");
        let mono = preview("JetBrains Mono", "Inter Sample", 16.0, 160, 22).expect("bundled mono");
        assert_eq!((sans.w, sans.h), (160, 22));
        assert!(ink(&sans) > 20.0, "{}", ink(&sans));
        assert_ne!(sans.a, mono.a, "each sample uses its own family's outlines");
        assert!(preview("No Such Family 1536", "No Such Family 1536", 16.0, 160, 22).is_none(), "an unknown family has no sample, not Inter's");
        assert!(preview("Inter", "日本語", 16.0, 160, 22).is_none(), "a face without the glyphs has no sample");
    }

    /// Browsing the font menu reads each installed face for its sample and drops it.
    #[test]
    fn previews_leave_installed_faces_unloaded() {
        // A face of its own: the installed ones are shared with tests that load every system face
        // while probing for a Japanese font.
        let path = std::env::temp_dir().join(format!("filmcraft-preview-{}.ttf", std::process::id()));
        std::fs::write(&path, fonts::INTER_REGULAR).expect("write temp font");
        let info = fonts::FaceInfo {
            family: "Inter".into(),
            style: "Regular".into(),
            weight: 400,
            italic: false,
            index: 0,
            data: fonts::FaceData::File(path.clone()),
            origin: "system",
        };
        let f = fonts::Face::unregistered(info);
        let drawn = sample(&f, "Aa", 16.0, 60, 22);
        let _ = std::fs::remove_file(&path);
        assert!(drawn.is_some_and(|m| ink(&m) > 5.0), "the sample is drawn from the file");
        assert!(!f.is_loaded(), "the file is not kept after the sample");
    }

    #[test]
    fn rotated_text_draws() {
        let l = layout("ROT", &TextStyle { size: 40.0, ..Default::default() }, &ParagraphStyle::default());
        let a = 30f32.to_radians();
        let (m, _, _) = rasterize(&l, &[a.cos(), a.sin(), -a.sin(), a.cos(), 0.0, 0.0]);
        let (m0, _, _) = rasterize(&l, &IDENTITY);
        assert!((ink(&m) - ink(&m0)).abs() / ink(&m0) < 0.03);
    }
}
