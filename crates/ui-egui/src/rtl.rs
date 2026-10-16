//! Right-to-left scripts (Arabic, Hebrew) in the interface: fallback fonts and reading order.
//!
//! Fonts: like [`crate::cjk`], the craft-fonts faces for these scripts (`Arab`, `Hebr`) become the
//! last fallbacks of every UI font family. A build without them looks up one installed face per
//! script once (Noto Sans Arabic, SF Arabic, Segoe UI…) and reuses it across theme changes.
//!
//! Reading order: egui shapes each font run with harfrust, so a word's letters join and run right
//! to left, but it places the runs left to right in logical order with no bidi pass: in
//! "كلمة أخرى" the second word comes out first. [`Lines`] lays out document text (captions) in
//! reading order for display. Lines are broken on the logical text as egui wraps it, each line's
//! runs are put in visual order by the Unicode Bidirectional Algorithm (UAX #9, as the text engine
//! does for titles), and every right-to-left word becomes its own layout section, which egui's
//! shaper draws right to left. Text being edited keeps egui's logical order: `TextEdit` maps the
//! caret by character index, so a reordered galley would put it in the wrong place.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use egui::epaint::text::{ByteIndex, LayoutJob, LayoutSection, TextFormat};
use egui::{Color32, FontData, FontDefinitions, FontId, Galley, Painter, Rect, Vec2, pos2};
use filmcraft_text::fonts::{CraftFont, Face};
use unicode_bidi::{BidiClass, BidiInfo, Level, ParagraphInfo, bidi_class};

/// A right-to-left script: its ISO 15924 code (as in the craft-fonts manifest), letters a face
/// must cover, and installed families to prefer, best first.
struct Script {
    code: &'static str,
    sample: &'static str,
    preferred: &'static [&'static str],
}

const SCRIPTS: &[Script] = &[
    Script {
        code: "Arab",
        sample: "ابتثجحخدذرزسشصضطظعغفقكلمنهويءآأؤإئةى،؟",
        preferred: &[
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
    },
    Script {
        code: "Hebr",
        sample: "אבגדהוזחטיכךלמםנןסעפףצץקרשת",
        preferred: &["Noto Sans Hebrew", "SF Hebrew", "Arial Hebrew", "Segoe UI", "Arial", "Tahoma", "DejaVu Sans"],
    },
];

/// The craft-fonts faces for right-to-left scripts (empty unless built with `CRAFT_FONTS_DIR`).
pub(crate) fn craft_fonts() -> impl Iterator<Item = &'static CraftFont> {
    filmcraft_text::fonts::CRAFT_FONTS.iter().filter(|f| f.scripts.iter().any(|s| SCRIPTS.iter().any(|x| x.code == *s)))
}

/// Installed faces for the scripts craft-fonts does not cover, looked up once per process (native
/// builds only). A face that covers several scripts (Segoe UI) is added once.
fn system_fonts() -> &'static [(String, Arc<FontData>)] {
    static FONTS: OnceLock<Vec<(String, Arc<FontData>)>> = OnceLock::new();
    FONTS.get_or_init(|| {
        if cfg!(target_arch = "wasm32") {
            return Vec::new();
        }
        let missing: Vec<&Script> = SCRIPTS.iter().filter(|s| !craft_fonts().any(|f| f.scripts.contains(&s.code))).collect();
        if missing.is_empty() {
            return Vec::new();
        }
        filmcraft_text::fonts::scan_system();
        let faces: Vec<_> = filmcraft_text::fonts::all_faces().into_iter().filter(|f| f.info.origin == "system" && !f.info.italic).collect();
        let by_weight = |f: &&Arc<Face>| f.info.weight.abs_diff(400);
        let mut chosen: Vec<&Arc<Face>> = Vec::new();
        let mut out = Vec::new();
        for s in missing {
            if chosen.iter().any(|f| f.covers_text(s.sample)) {
                continue;
            }
            let covers = |f: &&Arc<Face>| f.covers_text(s.sample);
            let preferred =
                s.preferred.iter().find_map(|name| faces.iter().filter(|f| f.info.family.eq_ignore_ascii_case(name)).filter(covers).min_by_key(by_weight));
            let Some(face) = preferred.or_else(|| faces.iter().filter(covers).min_by_key(by_weight)) else {
                continue;
            };
            let Some(data) = face.data() else {
                continue;
            };
            let font = FontData { font: Cow::Owned(data), index: face.info.index, tweak: Default::default() };
            out.push((format!("system-{}", s.code.to_lowercase()), Arc::new(font)));
            chosen.push(face);
        }
        out
    })
}

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
    for (name, font) in system_fonts() {
        fonts.font_data.insert(name.clone(), font.clone());
        names.push(name.clone());
    }
    for stack in fonts.families.values_mut() {
        for name in &names {
            if !stack.contains(name) {
                stack.push(name.clone());
            }
        }
    }
}

/// Whether an Arabic face is among the fallback faces [`install`] adds: a craft-fonts `Arab`
/// face, or an installed one found by [`system_fonts`].
pub(crate) fn arabic_font_available() -> bool {
    craft_fonts().any(|f| f.scripts.contains(&"Arab")) || system_fonts().iter().any(|(name, _)| name == "system-arab")
}

/// Whether `name` is an installed right-to-left face added by [`install`].
#[cfg(test)]
pub(crate) fn is_system_face(name: &str) -> bool {
    system_fonts().iter().any(|(n, _)| n == name)
}

/// Whether `text` has a right-to-left letter (Arabic, Hebrew…), the only text [`Lines`] reorders.
pub(crate) fn has_rtl(text: &str) -> bool {
    text.chars().any(|c| matches!(bidi_class(c), BidiClass::R | BidiClass::AL))
}

/// Characters that stay together, in logical order, in a right-to-left word: letters, marks and
/// joiners. egui shapes such a word as one right-to-left run.
fn in_word(c: char) -> bool {
    matches!(bidi_class(c), BidiClass::R | BidiClass::AL | BidiClass::NSM | BidiClass::BN)
}

/// A bracket shown right to left takes its mirrored glyph (UAX #9 rule L4).
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        _ => c,
    }
}

/// The layout sections of one line of `bidi.text` (byte range `line`, inside one paragraph), left
/// to right. A left-to-right run stays whole, except that Arabic-Indic digits get a section each
/// so the shaper cannot run them right to left. A right-to-left run is split into words
/// ([`in_word`]) and single other characters (mirrored), listed in reverse: egui shapes each word
/// right to left itself.
fn line_sections(bidi: &BidiInfo<'_>, line: Range<usize>) -> Vec<String> {
    let text = bidi.text;
    let Some(whole) = text.get(line.clone()).filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let Some(para) = bidi.paragraphs.iter().find(|p| p.range.start <= line.start && line.end <= p.range.end) else {
        return vec![whole.to_owned()];
    };
    let (levels, runs) = bidi.visual_runs(para, line);
    let mut out = Vec::new();
    for run in runs {
        let Some(s) = text.get(run.clone()) else {
            continue;
        };
        if levels.get(run.start).is_some_and(|l| l.is_rtl()) {
            let mut units = Vec::new();
            let mut word = String::new();
            for c in s.chars() {
                if in_word(c) {
                    word.push(c);
                    continue;
                }
                if !word.is_empty() {
                    units.push(std::mem::take(&mut word));
                }
                units.push(mirror(c).to_string());
            }
            if !word.is_empty() {
                units.push(word);
            }
            out.extend(units.into_iter().rev());
        } else {
            let mut plain = String::new();
            for c in s.chars() {
                if bidi_class(c) == BidiClass::AN {
                    if !plain.is_empty() {
                        out.push(std::mem::take(&mut plain));
                    }
                    out.push(c.to_string());
                } else {
                    plain.push(c);
                }
            }
            if !plain.is_empty() {
                out.push(plain);
            }
        }
    }
    out
}

/// Byte ranges of the lines egui broke `text` into in `galley` (newlines left out), or None when
/// the rows do not account for every character.
fn line_ranges(text: &str, galley: &Galley) -> Option<Vec<Range<usize>>> {
    let bounds: Vec<usize> = text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len())).collect();
    let mut out = Vec::with_capacity(galley.rows.len());
    let mut at = 0usize;
    for row in &galley.rows {
        let end = at.checked_add(row.glyphs.len())?;
        out.push(*bounds.get(at)?..*bounds.get(end)?);
        at = end.checked_add(usize::from(row.ends_with_newline))?;
    }
    (at == bounds.len().saturating_sub(1)).then_some(out)
}

/// Byte ranges of the lines of `text` between newlines.
fn paragraph_ranges(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            out.push(start..i);
            start = i + 1;
        }
    }
    out.push(start..text.len());
    out
}

/// Document text laid out for display in reading order, one galley per line.
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
        let logical = painter.layout(text.to_owned(), font.clone(), color, wrap_width);
        if !has_rtl(text) {
            return Self { size: logical.size(), rows: vec![logical], rtl: false };
        }
        let bidi = BidiInfo::new(text, None);
        let rtl = bidi.paragraphs.first().is_some_and(|p| p.level.is_rtl());
        let lines = line_ranges(text, &logical).unwrap_or_else(|| paragraph_ranges(text));
        let rows: Vec<Arc<Galley>> = lines
            .into_iter()
            .map(|line| {
                let mut job = LayoutJob::default();
                for s in line_sections(&bidi, line) {
                    let start = job.text.len();
                    job.text.push_str(&s);
                    // pushed, not appended: `append` merges sections of equal format, and each
                    // right-to-left word must be shaped on its own
                    job.sections.push(LayoutSection {
                        leading_space: 0.0,
                        byte_range: ByteIndex(start)..ByteIndex(job.text.len()),
                        format: TextFormat::simple(font.clone(), color),
                    });
                }
                if job.sections.is_empty() { painter.layout_no_wrap(String::new(), font.clone(), color) } else { painter.layout_job(job) }
            })
            .collect();
        let size = rows.iter().fold(Vec2::ZERO, |s, g| Vec2::new(s.x.max(g.size().x), s.y + g.size().y));
        Self { rows, rtl, size }
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

// Single-line display order. [`Lines`] lays document text out paragraph by paragraph; the
// helpers below produce a reordered *string* for places that hand egui one line to draw — a
// right-to-left interface language's labels ([`display_rtl`], applied to the catalog at load)
// and user text that may hold right-to-left script, like clip or project names ([`visual`]).

/// `text` rearranged for an egui call that lays out one line, base direction left to right as the
/// interface is: a file or project name keeps its extension and trailing parts where a reader of
/// the line expects them. Text without right-to-left characters comes back borrowed. Paint only —
/// stored text and automation labels keep the logical order.
pub(crate) fn visual(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_rtl_script) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    push_visual(&mut out, text, Level::ltr());
    Cow::Owned(out)
}

/// `text` in display order for the paragraph direction `base`, appended to `out`.
fn push_visual(out: &mut String, text: &str, base: Level) {
    let info = BidiInfo::new(text, Some(base));
    for para in &info.paragraphs {
        push_line(out, text, &info, para, para.range.clone());
    }
}

/// The `line` of paragraph `para` (a non-empty byte range of `text`) in display order, as egui
/// needs it, appended to `out`.
fn push_line(out: &mut String, text: &str, info: &BidiInfo<'_>, para: &ParagraphInfo, line: Range<usize>) {
    let (levels, runs) = info.visual_runs(para, line);
    // First the true display order, every character left to right as it appears on screen.
    let mut shown = String::with_capacity(text.len());
    for run in runs {
        let rtl = levels.get(run.start).is_some_and(Level::is_rtl);
        // Runs lie on character boundaries; a missing slice is skipped, never a panic.
        let Some(piece) = text.get(run) else { continue };
        let chars = piece.chars().filter(|c| !is_bidi_control(*c));
        if rtl {
            shown.extend(chars.rev().map(mirror));
        } else {
            shown.extend(chars);
        }
    }
    push_for_shaper(out, &shown);
}

/// `shown` (in screen order) as egui must get it: egui hands each stretch of right-to-left
/// script to the shaper as one run, which lays it out right to left (and joins its letters), so
/// each such stretch goes back to logical order. What lies between them (spaces, punctuation,
/// Latin) is drawn by the Latin face left to right and stays as it is. The stretches include
/// Persian and Arabic-Indic digits: Arabic script to the shaper, though they read left to right.
fn push_for_shaper(out: &mut String, shown: &str) {
    let mut stretch: Vec<char> = Vec::new();
    for c in shown.chars() {
        if is_rtl_script(c) || is_joiner(c) {
            stretch.push(c);
        } else {
            out.extend(stretch.drain(..).rev());
            out.push(c);
        }
    }
    out.extend(stretch.drain(..).rev());
}

/// A label of a right-to-left interface language, in logical order, rearranged for egui: each
/// line reads right to left, with Latin words and numbers inside it left to right.
///
/// - `{name}` placeholders stay whole and unmirrored, so [`crate::i18n::fmt`] still fills them.
///   What it inserts is drawn left to right at that place.
/// - Spaces and separator dots at either end stay at that end: the code appends to a label on
///   its right, as the interface is still laid out left to right.
/// - A short caption's closing colon stays on the right too, next to the widget it captions.
///
/// Text without right-to-left characters comes back unchanged. Like [`visual`], this is for
/// painting one line; a label egui wraps breaks at the wrong end.
pub(crate) fn display_rtl(text: &str) -> String {
    if !text.chars().any(is_rtl_script) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        push_rtl_line(&mut out, line);
    }
    out
}

/// The most words a caption may have for its closing colon to stay on the right.
const CAPTION_WORDS: usize = 3;
/// First of the private-use characters that stand for placeholders while a line is reordered.
/// They are strong left-to-right, like the names they replace.
const PLACEHOLDER_BASE: u32 = 0xE000;
/// Placeholders protected per line; a line with more keeps the rest as plain text.
const MAX_PLACEHOLDERS: usize = 64;

fn push_rtl_line(out: &mut String, line: &str) {
    let edge = |c: char| c.is_whitespace() || c == '·';
    let core = line.trim_start_matches(edge);
    let lead = line.get(..line.len().saturating_sub(core.len())).unwrap_or_default();
    let trimmed = core.trim_end_matches(edge);
    let tail = core.get(trimmed.len()..).unwrap_or_default();
    let mut core = trimmed;
    let mut colon = "";
    if let Some(caption) = core.strip_suffix(':')
        && caption.split_whitespace().count() <= CAPTION_WORDS
    {
        core = caption;
        colon = ":";
    }
    // A line that already uses the stand-in characters is reordered with its placeholders as text.
    let reserved = core.chars().any(|c| (PLACEHOLDER_BASE..PLACEHOLDER_BASE.saturating_add(MAX_PLACEHOLDERS as u32)).contains(&u32::from(c)));
    let (masked, names) = if reserved { (core.to_owned(), Vec::new()) } else { mask_placeholders(core) };
    let mut visual = String::with_capacity(masked.len());
    push_visual(&mut visual, &masked, Level::rtl());
    out.push_str(lead);
    for c in visual.chars() {
        let index = u32::from(c).checked_sub(PLACEHOLDER_BASE).and_then(|i| usize::try_from(i).ok());
        match index.and_then(|i| names.get(i)) {
            Some(name) => out.push_str(name),
            None => out.push(c),
        }
    }
    out.push_str(colon);
    out.push_str(tail);
}

/// `text` with each `{name}` placeholder and each keyboard shortcut (`⇧⌘+`) replaced by one
/// stand-in character, and what they replaced, in order. A shortcut's symbols have no direction
/// of their own, so left alone they would be drawn in reverse.
fn mask_placeholders(text: &str) -> (String, Vec<&str>) {
    let mut masked = String::with_capacity(text.len());
    let mut names: Vec<&str> = Vec::new();
    let mut rest = text;
    while names.len() < MAX_PLACEHOLDERS
        && let Some((start, end)) = next_atom(rest)
    {
        let (Some(before), Some(atom), Some(after)) = (rest.get(..start), rest.get(start..end), rest.get(end..)) else { break };
        // `names.len()` is below MAX_PLACEHOLDERS, so the sum is a private-use scalar.
        let Some(stand_in) = u32::try_from(names.len()).ok().and_then(|i| char::from_u32(PLACEHOLDER_BASE.saturating_add(i))) else { break };
        masked.push_str(before);
        masked.push(stand_in);
        names.push(atom);
        rest = after;
    }
    masked.push_str(rest);
    (masked, names)
}

/// The byte range of the first placeholder or keyboard shortcut in `text`.
fn next_atom(text: &str) -> Option<(usize, usize)> {
    let placeholder = text.find('{').and_then(|open| {
        let len = text.get(open..)?.find('}')?;
        Some((open, open.saturating_add(len).saturating_add(1)))
    });
    let shortcut = text.find(is_modifier_key).map(|at| {
        // The shortcut is the stretch around the symbol up to a space, a bracket or script text.
        let outside = |c: char| c.is_whitespace() || is_rtl_script(c) || matches!(c, '(' | ')' | '[' | ']' | '{' | '}');
        let start = text
            .get(..at)
            .and_then(|before| before.rfind(outside).and_then(|i| before.get(i..)?.chars().next().map(|c| i.saturating_add(c.len_utf8()))))
            .unwrap_or(0);
        let end = text.get(at..).and_then(|after| after.find(outside)).map_or(text.len(), |i| at.saturating_add(i));
        (start, end)
    });
    match (placeholder, shortcut) {
        (Some(p), Some(s)) => Some(if s.0 < p.0 { s } else { p }),
        (p, s) => p.or(s),
    }
}

/// The modifier-key symbols of keyboard shortcuts: ⌘ ⇧ ⌥ ⌃.
fn is_modifier_key(c: char) -> bool {
    matches!(c, '\u{2318}' | '\u{21E7}' | '\u{2325}' | '\u{2303}')
}

/// Characters of the right-to-left scripts' blocks: letters, marks and the scripts' own
/// punctuation, all drawn by the script's face.
fn is_rtl_script(c: char) -> bool {
    matches!(u32::from(c), 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFC | 0x1_0800..=0x1_0FFF | 0x1_E800..=0x1_EFFF)
}

/// The zero-width (non-)joiners inside a word (Persian `می‌روم`): egui shapes them with the
/// letter before them.
fn is_joiner(c: char) -> bool {
    matches!(c, '\u{200C}' | '\u{200D}')
}

/// Direction marks, embeddings, overrides and isolates: they steer the reordering and are not drawn.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sections of each line of `text` (split at newlines), left to right.
    fn sections(text: &str) -> Vec<Vec<String>> {
        let bidi = BidiInfo::new(text, None);
        paragraph_ranges(text).into_iter().map(|l| line_sections(&bidi, l)).collect()
    }

    #[test]
    fn rtl_words_are_listed_last_word_first() {
        assert_eq!(sections("كلمة أخرى"), [["أخرى", " ", "كلمة"]]);
        assert_eq!(sections("שלום עולם"), [["עולם", " ", "שלום"]]);
    }

    #[test]
    fn marks_and_joiners_stay_in_their_word() {
        // fatha, shadda and a zero-width non-joiner belong to the letters around them
        assert_eq!(sections("كَتَبَ مُحَمَّد"), [["مُحَمَّد", " ", "كَتَبَ"]]);
        assert_eq!(sections("می\u{200c}خواهم بود"), [["بود", " ", "می\u{200c}خواهم"]]);
    }

    #[test]
    fn latin_and_numbers_inside_rtl_keep_their_order() {
        assert_eq!(sections("قال FilmCraft 0.6 جيد"), [["جيد", " ", "FilmCraft 0.6", " ", "قال"]]);
        // Arabic-Indic digits read left to right, one section each so the shaper keeps them so
        assert_eq!(sections("سنة ١٤٤٥"), [["١", "٤", "٤", "٥", " ", "سنة"]]);
    }

    #[test]
    fn rtl_punctuation_and_brackets() {
        assert_eq!(sections("نعم، (نص)"), [["(", "نص", ")", " ", "،", "نعم"]]);
        assert_eq!(sections("لماذا؟"), [["لماذا؟"]]);
    }

    #[test]
    fn ltr_paragraph_with_an_rtl_phrase() {
        assert_eq!(sections("Lesson كلمة أخرى end"), [["Lesson ", "أخرى", " ", "كلمة", " end"]]);
        assert!(!has_rtl("Lesson 3: (draft)"));
        assert!(has_rtl("Lesson كلمة"));
    }

    #[test]
    fn each_line_is_reordered_on_its_own() {
        assert_eq!(sections("سطر أول\nسطر ثان"), [["أول", " ", "سطر"], ["ثان", " ", "سطر"]]);
        assert_eq!(sections("نص\n\nEnglish"), [vec!["نص"], vec![], vec!["English"]]);
    }

    #[test]
    fn hostile_ranges_give_nothing() {
        let text = "كلمة";
        let bidi = BidiInfo::new(text, None);
        assert!(line_sections(&bidi, 1..3).is_empty(), "not on a char boundary");
        assert!(line_sections(&bidi, 0..99).is_empty(), "past the end");
        #[allow(clippy::reversed_empty_ranges)]
        let backwards = 4..2;
        assert!(line_sections(&bidi, backwards).is_empty());
    }

    /// With an Arabic face installed, captions lay out with real glyphs in reading order: in
    /// "كلمة أخرى" the whole second word draws left of the first, its last letter leftmost.
    #[test]
    fn lines_draw_arabic_in_reading_order() {
        let ctx = egui::Context::default();
        let mut fonts = FontDefinitions::default();
        install(&mut fonts);
        if !craft_fonts().any(|f| f.scripts.contains(&"Arab")) && !system_fonts().iter().any(|(name, _)| name == "system-arab") {
            eprintln!("SKIPPED: no craft-fonts or installed Arabic font");
            return;
        }
        ctx.set_fonts(fonts);
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let painter = Painter::new(ctx.clone(), egui::LayerId::background(), Rect::EVERYTHING);
        let font = FontId::proportional(13.0);
        let lines = Lines::new(&painter, "كلمة أخرى", font.clone(), Color32::WHITE, f32::INFINITY);
        assert!(lines.rtl);
        let row = lines.rows.first().and_then(|g| g.rows.first()).map(|r| r.glyphs.clone()).unwrap_or_default();
        // egui shapes each word on its own (letters joined, in visual order) and pads the row with
        // empty glyphs so every char keeps a cursor position: only drawn glyphs count here
        let drawn: Vec<_> = row.iter().filter(|g| !g.uv_rect.is_nothing()).collect();
        let order = drawn.iter().map(|g| g.chr).collect::<String>();
        let xs = |word: &str| drawn.iter().filter(|g| word.contains(g.chr)).map(|g| g.pos.x).collect::<Vec<f32>>();
        let (first, second) = (xs("كلمة"), xs("أخرى"));
        assert!(!first.is_empty() && !second.is_empty(), "{order:?}");
        assert!(second.iter().fold(f32::MIN, |a, &b| a.max(b)) < first.iter().fold(f32::MAX, |a, &b| a.min(b)), "{order:?}");
        let leftmost = drawn.iter().min_by(|a, b| a.pos.x.total_cmp(&b.pos.x)).map(|g| g.chr);
        assert_eq!(leftmost, Some('ى'), "{order:?}");
        let missing = painter.layout_no_wrap("\u{fffd}".into(), font, Color32::WHITE);
        let missing_uv = missing.rows.first().and_then(|r| r.glyphs.first()).map(|g| g.uv_rect);
        assert!(drawn.iter().all(|g| Some(g.uv_rect) != missing_uv), "an Arabic letter drew as a missing glyph");
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
    }

    /// Lines broken by wrapping are each reordered: the first line holds the first words.
    #[test]
    fn line_ranges_follow_the_rows() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let painter = Painter::new(ctx.clone(), egui::LayerId::background(), Rect::EVERYTHING);
        let text = "one two three\nfour";
        let galley = painter.layout(text.to_owned(), FontId::proportional(13.0), Color32::WHITE, 40.0);
        let ranges = line_ranges(text, &galley).unwrap_or_default();
        assert!(ranges.len() >= 3, "{ranges:?}");
        let joined: Vec<&str> = ranges.iter().filter_map(|r| text.get(r.clone())).collect();
        assert_eq!(joined.concat().replace(' ', ""), "onetwothreefour");
        assert_eq!(ranges.last().and_then(|r| text.get(r.clone())), Some("four"));
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
    }

    #[test]
    fn text_without_rtl_is_borrowed_unchanged() {
        for s in ["", "report.mp4", "日本語の文字.mp4", "a (b) [c]", "caf\u{00E9} \u{200E}x"] {
            assert!(matches!(visual(s), Cow::Borrowed(v) if v == s), "{s:?}");
        }
    }

    #[test]
    fn arabic_words_swap_and_the_extension_stays_last() {
        // The reported name: "one two.mp4" in Arabic.
        assert_eq!(visual("واحد اثنين.mp4"), "اثنين واحد.mp4");
        assert_eq!(visual("ملف.mp4"), "ملف.mp4");
        assert_eq!(visual("تقرير نهائي جدا.mp4"), "جدا نهائي تقرير.mp4");
    }

    #[test]
    fn latin_and_numbers_keep_their_own_order() {
        assert_eq!(visual("report واحد اثنين final.mp4"), "report اثنين واحد final.mp4");
        // A number inside right-to-left text reads left to right, to the left of what precedes it.
        assert_eq!(visual("ملف 2024.mp4"), "2024 ملف.mp4");
        assert_eq!(visual(r"C:\Users\me\واحد اثنين\a.mp4"), r"C:\Users\me\اثنين واحد\a.mp4");
    }

    #[test]
    fn hebrew_and_brackets_display() {
        assert_eq!(visual("שלום עולם.mp4"), "עולם שלום.mp4");
        // Brackets between right-to-left words flip with the direction.
        assert_eq!(visual("واحد (اثنين) ثلاثة.mp4"), "ثلاثة (اثنين) واحد.mp4");
    }

    #[test]
    fn direction_controls_are_not_drawn() {
        assert_eq!(visual("\u{202B}واحد اثنين\u{202C}.mp4"), "اثنين واحد.mp4");
        assert_eq!(visual("\u{200F}واحد\u{061C}"), "واحد");
    }

    #[test]
    fn rtl_labels_read_right_to_left() {
        // No right-to-left characters: unchanged.
        for s in ["", "Save as…", "{n} clips", "日本語"] {
            assert_eq!(display_rtl(s), s);
        }
        assert_eq!(display_rtl("حفظ"), "حفظ");
        // "Save as…": the second word and the ellipsis end up on the left.
        assert_eq!(display_rtl("حفظ باسم…"), "…باسم حفظ");
        // Latin words and numbers keep their own order inside the line.
        assert_eq!(display_rtl("إنشاء تسلسل من مقطع"), "مقطع من تسلسل إنشاء");
        assert_eq!(display_rtl("صالح لمدة 5 سنوات"), "سنوات 5 لمدة صالح");
        // Keyboard shortcuts are not reversed.
        assert_eq!(display_rtl("طباعة (⌘P)"), "(⌘P) طباعة");
        assert_eq!(display_rtl("نص (عادي)"), "(عادي) نص");
    }

    #[test]
    fn rtl_labels_keep_placeholders_whole() {
        // Two placeholders around a colon stay together, left to right: a file name and its error.
        assert_eq!(display_rtl("تعذّر فتح {name}: {e}"), "{name}: {e} فتح تعذّر");
        assert_eq!(display_rtl("فشل {label}\u{060C} السبب {e}"), "{e} السبب \u{060C}{label} فشل");
        assert_eq!(display_rtl("{n} مقاطع"), "مقاطع {n}");
        // Adjacent placeholders keep their order, as numbers around a slash would.
        assert_eq!(display_rtl("جارٍ البحث… {s}/{p}"), "{s}/{p} …البحث جارٍ");
        // Braces that are not a placeholder are ordinary punctuation.
        assert_eq!(display_rtl("حرف } واحد"), "واحد { حرف");
        let many = format!("ا {}", "{a} ".repeat(200));
        assert_eq!(display_rtl(&many).matches("{a}").count(), 200);
    }

    #[test]
    fn rtl_labels_keep_their_edges() {
        // Edge spaces and separator dots stay where the code concatenates.
        assert_eq!(display_rtl(" · محمي بكلمة مرور"), " · مرور بكلمة محمي");
        // A caption's colon stays next to its widget; a sentence's colon ends the sentence.
        assert_eq!(display_rtl("الاسم:"), "الاسم:");
        assert_eq!(display_rtl("اختر المعرّف الرقمي الذي تريد استخدامه:"), ":استخدامه تريد الذي الرقمي المعرّف اختر");
        // Each line is its own paragraph.
        assert_eq!(display_rtl("تعذّر عرض المقطع.\n{e}"), ".المقطع عرض تعذّر\n{e}");
    }

    #[test]
    fn rtl_labels_never_panic_on_odd_input() {
        let long = "ا {n} ".repeat(20_000);
        for s in [
            "ا{",
            "ا}",
            "ا{}",
            "ا{{{",
            "}ا{",
            "ا\u{E000}{n}",
            "⌘",
            "ا ⌘",
            "⌘ا⌘{",
            "ا (⇧⌘",
            "\u{064B}:",
            ":",
            "ا:",
            " : ا : ",
            "ا\n\n\nب",
            "\u{202E}ا{n}",
            long.as_str(),
        ] {
            let v = display_rtl(s);
            assert!(v.chars().count() <= s.chars().count(), "{:?}", s.chars().take(12).collect::<String>());
        }
    }

    #[test]
    fn odd_input_never_panics() {
        let long = "ا ب ".repeat(20_000);
        let odd = [
            "\u{064B}",
            "\u{064B}\u{064C} \u{0301}",
            "\u{202E}\u{202E}\u{202E}ا",
            "\u{2067}ا\u{2066}b",
            "ا\nب\r\nج\u{2029}د",
            "\u{FEFF}ا\u{0000}ب",
            "ا\u{10FFFF}\u{E000}ب",
            "((((ا]]]]",
            long.as_str(),
        ];
        for s in odd {
            let v = visual(s);
            assert!(v.chars().count() <= s.chars().count(), "{:?}", s.chars().take(12).collect::<String>());
        }
    }
}
