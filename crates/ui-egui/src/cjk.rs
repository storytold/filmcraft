//! Chinese fallback fonts for document text in the interface, independent of UI language.
//! Optional craft-fonts are preferred; native builds without them use one installed face, read
//! the first time the interface paints text that needs it ([`crate::fallback_fonts`]). The
//! installed Hangul face is read the same way.

use std::ops::RangeInclusive;
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

use crate::fallback_fonts::Fallback;

const SAMPLE: &str = "中文轨道配乐音效简体汉语繁體漢語车";
const PREFERRED: &[&str] = &[
    "Microsoft YaHei UI",
    "Microsoft YaHei",
    "PingFang SC",
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
    "Source Han Sans CN",
    "WenQuanYi Micro Hei",
    "Microsoft JhengHei UI",
    "Microsoft JhengHei",
];

pub(crate) fn craft_fonts() -> impl Iterator<Item = &'static filmcraft_text::fonts::CraftFont> {
    filmcraft_text::fonts::CRAFT_FONTS.iter().filter(|f| f.scripts.iter().any(|s| matches!(*s, "Hans" | "Hant")))
}

// Hangul: neither the UI fonts nor the Chinese/Japanese fallbacks have it, so typed, pasted and
// IME-composed Korean drew as missing-glyph boxes (artcraft#2093).
const KOREAN_SAMPLE: &str = "한국어입력테스트";
const KOREAN_PREFERRED: &[&str] = &["Malgun Gothic", "Apple SD Gothic Neo", "Noto Sans CJK KR", "Noto Sans KR", "Source Han Sans K", "NanumGothic"];

/// Han, kana, bopomofo and the CJK symbols, punctuation and full-width forms: what an installed
/// Chinese face draws for the interface.
const CJK_SCRIPT: &[RangeInclusive<char>] = &[
    '\u{2e80}'..='\u{2fdf}',
    '\u{2ff0}'..='\u{303f}',
    '\u{3040}'..='\u{312f}',
    '\u{3190}'..='\u{33ff}',
    '\u{3400}'..='\u{4dbf}',
    '\u{4e00}'..='\u{9fff}',
    '\u{f900}'..='\u{faff}',
    '\u{fe30}'..='\u{fe4f}',
    '\u{ff00}'..='\u{ffef}',
    '\u{1b000}'..='\u{1b16f}',
    '\u{20000}'..='\u{3ffff}',
];

/// Hangul: jamo, compatibility jamo, syllables, half-width forms, and the parenthesized and circled
/// Hangul of the enclosed CJK block.
const HANGUL_SCRIPT: &[RangeInclusive<char>] = &[
    '\u{1100}'..='\u{11ff}',
    '\u{3130}'..='\u{318f}',
    '\u{3200}'..='\u{321e}',
    '\u{3260}'..='\u{327e}',
    '\u{a960}'..='\u{a97f}',
    '\u{ac00}'..='\u{d7ff}',
    '\u{ffa0}'..='\u{ffdc}',
];

fn without_craft_chinese() -> bool {
    craft_fonts().next().is_none()
}

fn always() -> bool {
    true
}

/// The installed Chinese face, for builds without the craft-fonts Hans/Hant faces.
pub(crate) static CHINESE: Fallback = Fallback::new("system-chinese", SAMPLE, PREFERRED, CJK_SCRIPT, without_craft_chinese);

/// The font-data name of the installed Hangul face [`install`] appends to every family.
pub(crate) const KOREAN_FACE: &str = "system-korean";

/// The installed Hangul face (none is bundled).
pub(crate) static KOREAN: Fallback = Fallback::new(KOREAN_FACE, KOREAN_SAMPLE, KOREAN_PREFERRED, HANGUL_SCRIPT, always);

/// Whether [`install`] has a Hangul face to add (an installed system face; none is bundled).
/// The Korean interface needs one.
pub(crate) fn korean_available() -> bool {
    KOREAN.face.face().is_some()
}

/// Whether [`install`] has a Chinese face to add: craft-fonts Hans/Hant faces, or an installed
/// system face (scanned once per process). The Simplified Chinese interface needs one.
pub(crate) fn available() -> bool {
    craft_fonts().next().is_some() || CHINESE.face.face().is_some()
}

/// Append Chinese faces after the Latin UI fonts and the Japanese fallbacks. This keeps the
/// interface's Latin typography and Japanese glyph forms, and covers the simplified and
/// traditional hanzi in media and track names that Japanese fonts lack. Installed faces are
/// appended once read ([`crate::fallback_fonts`]).
pub(crate) fn install(fonts: &mut FontDefinitions) {
    install_chinese(fonts);
    if let Some(font) = KOREAN.font_data() {
        fonts.font_data.insert(KOREAN.name.into(), font);
        for stack in fonts.families.values_mut() {
            stack.push(KOREAN.name.into());
        }
    }
}

fn install_chinese(fonts: &mut FontDefinitions) {
    let chinese: Vec<_> = craft_fonts().collect();
    if chinese.is_empty() {
        if let Some(font) = CHINESE.font_data() {
            fonts.font_data.insert(CHINESE.name.into(), font);
            for stack in fonts.families.values_mut() {
                stack.push(CHINESE.name.into());
            }
        }
        return;
    }
    let name = |f: &filmcraft_text::fonts::CraftFont| format!("craft:{} {}", f.family, f.style);
    for f in &chinese {
        fonts.font_data.insert(name(f), Arc::new(FontData::from_static(f.bytes)));
    }
    for (family, stack) in &mut fonts.families {
        let heavy = matches!(family, FontFamily::Name(n) if matches!(n.as_ref(), "semibold" | "medium"));
        let mut order = chinese.clone();
        order.sort_by_key(|f| (f.family.contains("Serif") && !f.family.contains("Sans"), (f.style == "Bold") != heavy));
        stack.extend(order.iter().map(|f| name(f)));
    }
}

#[cfg(test)]
mod font_blob_tests {
    use super::*;

    /// `FontData::from_blob` retains the TTC face index when constructed from a shared blob.
    #[test]
    fn a_ttc_face_index_survives_font_data_construction() {
        let font = FontData::from_blob(Arc::new(vec![0u8; 4]), 7);
        assert_eq!(font.index, 7, "the face index must travel with the shared blob");
    }
}
