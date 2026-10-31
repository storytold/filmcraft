//! Right-to-left scripts (Arabic, Hebrew) in the interface: fallback fonts and reading order.
//!
//! Fonts: like [`crate::cjk`], the craft-fonts faces for these scripts (`Arab`, `Hebr`) become the
//! last fallbacks of every UI font family. A build without them uses one installed face per script
//! (Noto Sans Arabic, SF Arabic, Segoe UI…), read the first time the interface paints text in it
//! ([`crate::fallback_fonts`]).
//!
//! Reading order: the egui 0.37 fork performs UAX #9 shaping, run ordering and bracket mirroring.
//! [`Lines`] lays out document text (captions) with one logical `LayoutJob` and
//! `TextDirection::Auto`, so the text engine produces visual order itself. Keeping the old
//! word-by-word reversal would reverse Arabic/Hebrew twice. Text being edited keeps egui's logical
//! order: `TextEdit` maps the caret by character index.

use std::sync::Arc;

use egui::epaint::text::{LayoutJob, TextDirection, TextFormat};
use egui::{Align, Color32, FontData, FontDefinitions, FontId, Galley, Painter, Rect, Vec2, pos2};
use filmcraft_text::fonts::{CraftFont, Face};
use unicode_bidi::{BidiClass, BidiInfo, bidi_class};

use crate::fallback_fonts::Fallback;

/// The right-to-left scripts, by ISO 15924 code (as in the craft-fonts manifest).
const CODES: [&str; 2] = ["Arab", "Hebr"];

/// The craft-fonts faces for right-to-left scripts (empty unless built with `CRAFT_FONTS_DIR`).
pub(crate) fn craft_fonts() -> impl Iterator<Item = &'static CraftFont> {
    filmcraft_text::fonts::CRAFT_FONTS.iter().filter(|f| f.scripts.iter().any(|s| CODES.contains(s)))
}

fn without_craft(code: &str) -> bool {
    !craft_fonts().any(|f| f.scripts.contains(&code))
}

fn without_craft_arabic() -> bool {
    without_craft("Arab")
}

fn without_craft_hebrew() -> bool {
    without_craft("Hebr")
}

/// The installed Arabic face, for builds without the craft-fonts `Arab` face: Arabic, its
/// supplements and presentation forms.
pub(crate) static ARABIC: Fallback = Fallback::new(
    "system-arab",
    "ابتثجحخدذرزسشصضطظعغفقكلمنهويءآأؤإئةى،؟",
    &[
        "Noto Sans Arabic UI",
        "Noto Sans Arabic",
        "SF Arabic",
        "Geeza Pro",
        "Segoe UI",
        "Noto Naskh Arabic UI",
        "Noto Naskh Arabic",
        "Tahoma",
        "Arial",
        "DejaVu Sans",
    ],
    &['\u{600}'..='\u{6ff}', '\u{750}'..='\u{77f}', '\u{870}'..='\u{8ff}', '\u{fb50}'..='\u{fdff}', '\u{fe70}'..='\u{fefc}', '\u{1ee00}'..='\u{1eeff}'],
    without_craft_arabic,
);

/// The installed Hebrew face (craft-fonts has none).
pub(crate) static HEBREW: Fallback = Fallback::new(
    "system-hebr",
    "אבגדהוזחטיכךלמםנןסעפףצץקרשת",
    &["Noto Sans Hebrew", "SF Hebrew", "Arial Hebrew", "Segoe UI", "Arial", "Tahoma", "DejaVu Sans"],
    &['\u{590}'..='\u{5ff}', '\u{fb1d}'..='\u{fb4f}'],
    without_craft_hebrew,
);

/// Append the right-to-left faces (craft-fonts, else installed ones) after the other fallbacks of
/// every font family. Their letters are in no other UI font, so their place among the fallbacks
/// does not matter, and Latin text keeps the interface fonts. Installed faces come last, after
/// the craft-fonts faces: one may be a broad face (DejaVu Sans, Segoe UI) that should not take
/// symbols the craft-fonts faces draw.
pub(crate) fn install(fonts: &mut FontDefinitions) {
    let mut names = Vec::new();
    for f in craft_fonts() {
        let name = format!("craft:{} {}", f.family, f.style);
        fonts.font_data.insert(name.clone(), Arc::new(FontData::from_static(f.bytes)));
        names.push(name);
    }
    // installed faces once read ([`crate::fallback_fonts`]); a face that covers both scripts
    // (Segoe UI) is added once
    let mut added: Vec<&Arc<Face>> = Vec::new();
    for fallback in [&ARABIC, &HEBREW] {
        let (Some(font), Some(face)) = (fallback.font_data(), fallback.face.face()) else { continue };
        if added.iter().any(|f| Arc::ptr_eq(f, face)) {
            continue;
        }
        added.push(face);
        fonts.font_data.insert(fallback.name.into(), font);
        names.push(fallback.name.to_string());
    }
    for stack in fonts.families.values_mut() {
        for name in &names {
            if !stack.contains(name) {
                stack.push(name.clone());
            }
        }
    }
}

/// Whether `name` is an installed right-to-left face added by [`install`].
#[cfg(test)]
pub(crate) fn is_system_face(name: &str) -> bool {
    [&ARABIC, &HEBREW].iter().any(|f| f.name == name)
}

/// Whether `text` has a right-to-left letter (Arabic, Hebrew…), the only text [`Lines`] reorders.
pub(crate) fn has_rtl(text: &str) -> bool {
    text.chars().any(|c| matches!(bidi_class(c), BidiClass::R | BidiClass::AL))
}

/// Document text laid out for display in reading order by the fork's bidi pass.
pub(crate) struct Lines {
    rows: Vec<Arc<Galley>>,
    /// The text starts right to left (its first strong letter is Arabic or Hebrew): lines align right.
    pub rtl: bool,
    pub size: Vec2,
}

impl Lines {
    /// Lay out `text` in `font`, wrapped at `wrap_width` (`f32::INFINITY`: only at `\n`). Text
    /// without right-to-left letters is laid out as egui always does, in one galley.
    pub fn new(painter: &Painter, text: &str, font: FontId, color: Color32, wrap_width: f32) -> Self {
        // The egui fork performs UAX #9 paragraph direction, bidi runs, visual ordering and
        // bracket mirroring itself (`TextDirection::Auto`). Reversing words here as well would
        // reverse them twice; document text keeps one logical section and one layout job.
        let rtl = has_rtl(text) && BidiInfo::new(text, None).paragraphs.first().is_some_and(|p| p.level.is_rtl());
        let mut job = LayoutJob::default();
        job.wrap.max_width = wrap_width;
        job.direction = TextDirection::Auto;
        job.halign = if rtl { Align::RIGHT } else { Align::LEFT };
        job.halign_min_is_start = true;
        job.append(text, 0.0, TextFormat::simple(font, color));
        let galley = painter.layout_job(job);
        let size = galley.size();
        Self { rows: vec![galley], rtl, size }
    }

    /// Paint the lines down from the top of `rect`: from its left edge, or from its right edge when
    /// [`Self::rtl`].
    pub fn paint(&self, painter: &Painter, rect: Rect, color: Color32) {
        let mut y = rect.min.y;
        for g in &self.rows {
            let x = if self.rtl { rect.max.x - g.size().x } else { rect.min.x };
            painter.galley(pos2(x, y), g.clone(), color);
            y += g.size().y;
        }
    }
}

/// Paint one line of document text vertically centred in `rect`, in reading order: from the left
/// edge, or from the right edge when it reads right to left, so a clipped line still shows its start.
pub(crate) fn paint_line(painter: &Painter, rect: Rect, text: &str, font: FontId, color: Color32) {
    let lines = Lines::new(painter, text, font, color, f32::INFINITY);
    let top = rect.center().y - lines.size.y / 2.0;
    lines.paint(painter, Rect::from_min_max(pos2(rect.min.x, top), pos2(rect.max.x, top + lines.size.y)), color);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Diagnostic helper: glyph characters in visual order, with control characters filtered out.
    fn drawn_chars(row: &egui::epaint::text::Row) -> Vec<char> {
        row.visual_order().map(|i| row.glyphs[i].chr).filter(|c| !c.is_control()).collect()
    }

    /// A right-to-left paragraph sets both RTL flags and draws the second logical word first.
    #[test]
    fn rtl_paragraph_is_flagged_and_visually_ordered() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let painter = Painter::new(ctx.clone(), egui::LayerId::background(), Rect::EVERYTHING);
        let lines = Lines::new(&painter, "كلمة أخرى", FontId::proportional(13.0), Color32::WHITE, f32::INFINITY);
        assert!(lines.rtl);
        let row = lines.rows.first().and_then(|g| g.rows.first()).expect("one row");
        assert!(row.rtl_paragraph);
        let order: String = drawn_chars(row).into_iter().collect();
        // Glyphs retain logical characters; the visual iterator walks them left to right.
        // Both Arabic words therefore appear reversed in this diagnostic string.
        assert_eq!(order, "ىرخأ ةملك", "the second logical word must draw first");
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
    }

    /// Paragraph direction follows the first strong character; an embedded RTL phrase must not flip the line.
    #[test]
    fn ltr_paragraph_with_an_rtl_phrase_keeps_its_direction() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let painter = Painter::new(ctx.clone(), egui::LayerId::background(), Rect::EVERYTHING);
        let lines = Lines::new(&painter, "Lesson كلمة أخرى end", FontId::proportional(13.0), Color32::WHITE, f32::INFINITY);
        assert!(!lines.rtl);
        assert!(has_rtl("Lesson كلمة"));
        let row = lines.rows.first().and_then(|g| g.rows.first()).expect("one row");
        assert!(!row.rtl_paragraph);
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
    }

    /// Bidi classification remains total for Latin, punctuation and Arabic input.
    #[test]
    fn hostile_ranges_do_not_panic() {
        assert!(!has_rtl("Lesson 3: (draft)"));
        assert!(has_rtl("كلمة"));
    }

    /// With an Arabic face installed, captions lay out with real glyphs in reading order: in
    /// "كلمة أخرى" the whole second word draws left of the first, its last letter leftmost.
    #[test]
    fn lines_draw_arabic_in_reading_order() {
        let ctx = egui::Context::default();
        let mut fonts = FontDefinitions::default();
        // read as when the interface first paints Arabic
        let installed = ARABIC.read();
        install(&mut fonts);
        if !craft_fonts().any(|f| f.scripts.contains(&"Arab")) && !installed {
            eprintln!("SKIPPED: no craft-fonts or installed Arabic font");
            return;
        }
        ctx.set_fonts(fonts);
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let painter = Painter::new(ctx.clone(), egui::LayerId::background(), Rect::EVERYTHING);
        let font = FontId::proportional(13.0);
        let lines = Lines::new(&painter, "كلمة أخرى", font.clone(), Color32::WHITE, f32::INFINITY);
        assert!(lines.rtl);
        let row = lines.rows.first().and_then(|g| g.rows.first()).expect("one row");
        let drawn: Vec<_> = row.visual_order().map(|i| &row.glyphs[i]).filter(|g| !g.uv_rect.is_nothing()).collect();
        assert!(!drawn.is_empty(), "Arabic text must draw some glyphs");
        let xs = |word: &str| drawn.iter().filter(|g| word.contains(g.chr)).map(|g| g.pos.x).collect::<Vec<f32>>();
        let (first, second) = (xs("كلمة"), xs("أخرى"));
        assert!(!first.is_empty() && !second.is_empty());
        assert!(second.iter().fold(f32::MIN, |a, &b| a.max(b)) < first.iter().fold(f32::MAX, |a, &b| a.min(b)));
        let missing = painter.layout_no_wrap("\u{fffd}".into(), font, Color32::WHITE);
        let missing_uv = missing.rows.first().and_then(|r| r.glyphs.first()).map(|g| g.uv_rect);
        assert!(drawn.iter().all(|g| Some(g.uv_rect) != missing_uv), "an Arabic letter drew as a missing glyph");
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
    }
}
